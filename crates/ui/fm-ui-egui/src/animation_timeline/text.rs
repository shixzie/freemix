//! Wording for the animation timeline from the shared Studio catalog
//! ([`Msg`]): error messages keyed by [`AnimEditorError::msg_key`], names
//! and hover explanations for phases, properties, presets, and easings, and
//! values in operator units with the locale's number format.

use fm_animation::{AnimationPreset, Easing, EasingPreset, Phase, Property, Value};
use fm_ui_model::animation_editor::{AnimEditorError, AnimLimit, CurveControl, CurvePreset};
use fm_ui_model::i18n::{Arg, Language, Locale, Msg};

/// Display names of the sixteen garbage matte coordinates (point 1 x, point
/// 1 y, point 2 x, ...): only scene layers animate these.
const MATTE_NAMES: [&str; 16] = [
    "Matte point 1 X",
    "Matte point 1 Y",
    "Matte point 2 X",
    "Matte point 2 Y",
    "Matte point 3 X",
    "Matte point 3 Y",
    "Matte point 4 X",
    "Matte point 4 Y",
    "Matte point 5 X",
    "Matte point 5 Y",
    "Matte point 6 X",
    "Matte point 6 Y",
    "Matte point 7 X",
    "Matte point 7 Y",
    "Matte point 8 X",
    "Matte point 8 Y",
];

const MATTE_NAMES_ES: [&str; 16] = [
    "Punto de máscara 1 X",
    "Punto de máscara 1 Y",
    "Punto de máscara 2 X",
    "Punto de máscara 2 Y",
    "Punto de máscara 3 X",
    "Punto de máscara 3 Y",
    "Punto de máscara 4 X",
    "Punto de máscara 4 Y",
    "Punto de máscara 5 X",
    "Punto de máscara 5 Y",
    "Punto de máscara 6 X",
    "Punto de máscara 6 Y",
    "Punto de máscara 7 X",
    "Punto de máscara 7 Y",
    "Punto de máscara 8 X",
    "Punto de máscara 8 Y",
];

const MATTE_HOVERS: [&str; 16] = [
    "X of garbage matte point 1, from 0 at the layer's left edge to 1000 at its right. The layer must already have a matte with that point.",
    "Y of garbage matte point 1, from 0 at the layer's top edge to 1000 at its bottom. The layer must already have a matte with that point.",
    "X of garbage matte point 2, from 0 at the layer's left edge to 1000 at its right. The layer must already have a matte with that point.",
    "Y of garbage matte point 2, from 0 at the layer's top edge to 1000 at its bottom. The layer must already have a matte with that point.",
    "X of garbage matte point 3, from 0 at the layer's left edge to 1000 at its right. The layer must already have a matte with that point.",
    "Y of garbage matte point 3, from 0 at the layer's top edge to 1000 at its bottom. The layer must already have a matte with that point.",
    "X of garbage matte point 4, from 0 at the layer's left edge to 1000 at its right. The layer must already have a matte with that point.",
    "Y of garbage matte point 4, from 0 at the layer's top edge to 1000 at its bottom. The layer must already have a matte with that point.",
    "X of garbage matte point 5, from 0 at the layer's left edge to 1000 at its right. The layer must already have a matte with that point.",
    "Y of garbage matte point 5, from 0 at the layer's top edge to 1000 at its bottom. The layer must already have a matte with that point.",
    "X of garbage matte point 6, from 0 at the layer's left edge to 1000 at its right. The layer must already have a matte with that point.",
    "Y of garbage matte point 6, from 0 at the layer's top edge to 1000 at its bottom. The layer must already have a matte with that point.",
    "X of garbage matte point 7, from 0 at the layer's left edge to 1000 at its right. The layer must already have a matte with that point.",
    "Y of garbage matte point 7, from 0 at the layer's top edge to 1000 at its bottom. The layer must already have a matte with that point.",
    "X of garbage matte point 8, from 0 at the layer's left edge to 1000 at its right. The layer must already have a matte with that point.",
    "Y of garbage matte point 8, from 0 at the layer's top edge to 1000 at its bottom. The layer must already have a matte with that point.",
];

const MATTE_HOVERS_ES: [&str; 16] = [
    "X del punto 1 de la máscara de basura, de 0 en el borde izquierdo de la capa a 1000 en el derecho. La capa ya debe tener una máscara con ese punto.",
    "Y del punto 1 de la máscara de basura, de 0 en el borde superior de la capa a 1000 en el inferior. La capa ya debe tener una máscara con ese punto.",
    "X del punto 2 de la máscara de basura, de 0 en el borde izquierdo de la capa a 1000 en el derecho. La capa ya debe tener una máscara con ese punto.",
    "Y del punto 2 de la máscara de basura, de 0 en el borde superior de la capa a 1000 en el inferior. La capa ya debe tener una máscara con ese punto.",
    "X del punto 3 de la máscara de basura, de 0 en el borde izquierdo de la capa a 1000 en el derecho. La capa ya debe tener una máscara con ese punto.",
    "Y del punto 3 de la máscara de basura, de 0 en el borde superior de la capa a 1000 en el inferior. La capa ya debe tener una máscara con ese punto.",
    "X del punto 4 de la máscara de basura, de 0 en el borde izquierdo de la capa a 1000 en el derecho. La capa ya debe tener una máscara con ese punto.",
    "Y del punto 4 de la máscara de basura, de 0 en el borde superior de la capa a 1000 en el inferior. La capa ya debe tener una máscara con ese punto.",
    "X del punto 5 de la máscara de basura, de 0 en el borde izquierdo de la capa a 1000 en el derecho. La capa ya debe tener una máscara con ese punto.",
    "Y del punto 5 de la máscara de basura, de 0 en el borde superior de la capa a 1000 en el inferior. La capa ya debe tener una máscara con ese punto.",
    "X del punto 6 de la máscara de basura, de 0 en el borde izquierdo de la capa a 1000 en el derecho. La capa ya debe tener una máscara con ese punto.",
    "Y del punto 6 de la máscara de basura, de 0 en el borde superior de la capa a 1000 en el inferior. La capa ya debe tener una máscara con ese punto.",
    "X del punto 7 de la máscara de basura, de 0 en el borde izquierdo de la capa a 1000 en el derecho. La capa ya debe tener una máscara con ese punto.",
    "Y del punto 7 de la máscara de basura, de 0 en el borde superior de la capa a 1000 en el inferior. La capa ya debe tener una máscara con ese punto.",
    "X del punto 8 de la máscara de basura, de 0 en el borde izquierdo de la capa a 1000 en el derecho. La capa ya debe tener una máscara con ese punto.",
    "Y del punto 8 de la máscara de basura, de 0 en el borde superior de la capa a 1000 en el inferior. La capa ya debe tener una máscara con ese punto.",
];

/// The sentence for a core error id in `locale`, or `None` for an unknown
/// id.
#[must_use]
pub fn error_message(locale: Locale, msg_key: &str) -> Option<&'static str> {
    Msg::anim_editor_error(msg_key).map(|msg| locale.text(msg))
}

/// The full inline message for `error`, with the detail an operator can act
/// on (the shortest length that fits, the colliding frame, the limit).
#[must_use]
pub fn describe_error<T>(locale: Locale, error: &AnimEditorError<T>) -> String {
    let message =
        error_message(locale, error.msg_key()).unwrap_or_else(|| locale.text(Msg::AnimErrRefused));
    let message = ("message", Arg::Str(message));
    match error {
        AnimEditorError::RetimeCollision { min_len, .. } => locale.format(
            Msg::AnimErrShortestFits,
            &[
                message,
                ("length", Arg::Str(&crate::l10n::frames(locale, *min_len))),
            ],
        ),
        AnimEditorError::KeyCollision { frame, .. }
        | AnimEditorError::KeyOutsidePhase { frame, .. } => locale.format(
            Msg::AnimErrAtFrame,
            &[message, ("frame", Arg::from(*frame))],
        ),
        AnimEditorError::LimitExceeded(limit) => {
            let (msg, max) = match limit {
                AnimLimit::PhaseFrames { max } => (Msg::AnimErrMaxPhaseFrames, Arg::from(*max)),
                AnimLimit::Tracks { max } => (Msg::AnimErrMaxTracks, Arg::from(*max)),
                AnimLimit::TrackKeys { max } => (Msg::AnimErrMaxTrackKeys, Arg::from(*max)),
                AnimLimit::ClipKeys { max } => (Msg::AnimErrMaxClipKeys, Arg::from(*max)),
            };
            locale.format(msg, &[message, ("max", max)])
        }
        _ => locale.format(Msg::AnimErrSentence, &[message]),
    }
}

/// Tab label of a phase.
#[must_use]
pub fn phase_label(locale: Locale, phase: Phase) -> &'static str {
    locale.text(match phase {
        Phase::In => Msg::AnimPhaseIn,
        Phase::Out => Msg::AnimPhaseOut,
    })
}

/// Hover explanation of a phase tab.
#[must_use]
pub fn phase_hover(locale: Locale, phase: Phase) -> &'static str {
    locale.text(match phase {
        Phase::In => Msg::AnimPhaseInHover,
        Phase::Out => Msg::AnimPhaseOutHover,
    })
}

/// Display name of a property.
#[must_use]
pub fn property_name(locale: Locale, property: Property) -> &'static str {
    if let Some(index) = property.matte_index() {
        return match locale.language {
            Language::Spanish => MATTE_NAMES_ES[index],
            Language::English => MATTE_NAMES[index],
        };
    }
    locale.text(match property {
        Property::PositionX => Msg::AnimPropPositionX,
        Property::PositionY => Msg::AnimPropPositionY,
        Property::ScaleX => Msg::AnimPropScaleX,
        Property::ScaleY => Msg::AnimPropScaleY,
        Property::Rotation => Msg::AnimPropRotation,
        Property::Opacity => Msg::AnimPropOpacity,
        Property::CropLeft => Msg::AnimPropCropLeft,
        Property::CropTop => Msg::AnimPropCropTop,
        Property::CropRight => Msg::AnimPropCropRight,
        Property::CropBottom => Msg::AnimPropCropBottom,
        Property::Color => Msg::AddInputColour,
        _ => unreachable!("every matte property has its own name above"),
    })
}

/// Hover explanation of a property row.
#[must_use]
pub fn property_hover(locale: Locale, property: Property) -> &'static str {
    if let Some(index) = property.matte_index() {
        return match locale.language {
            Language::Spanish => MATTE_HOVERS_ES[index],
            Language::English => MATTE_HOVERS[index],
        };
    }
    locale.text(match property {
        Property::PositionX => Msg::AnimPropPositionXHover,
        Property::PositionY => Msg::AnimPropPositionYHover,
        Property::ScaleX => Msg::AnimPropScaleXHover,
        Property::ScaleY => Msg::AnimPropScaleYHover,
        Property::Rotation => Msg::AnimPropRotationHover,
        Property::Opacity => Msg::AnimPropOpacityHover,
        Property::CropLeft => Msg::AnimPropCropLeftHover,
        Property::CropTop => Msg::AnimPropCropTopHover,
        Property::CropRight => Msg::AnimPropCropRightHover,
        Property::CropBottom => Msg::AnimPropCropBottomHover,
        Property::Color => Msg::AnimPropColourHover,
        _ => unreachable!("every matte property has its own hover above"),
    })
}

/// Unit suffix and divisor turning a stored scalar into the shown number.
#[must_use]
pub const fn property_unit(property: Property) -> (&'static str, f64) {
    if property.matte_index().is_some() {
        return ("", 1.0);
    }
    match property {
        Property::ScaleX | Property::ScaleY => ("%", 100.0),
        Property::Rotation => ("°", 1_000.0),
        Property::Opacity => ("%", 2.55),
        Property::Color => ("", 1.0),
        _ => (" px", 1.0),
    }
}

/// A shown number with up to `places` decimals in the locale's format, plus
/// `unit`.
#[must_use]
pub fn format_number(locale: Locale, shown: f64, places: usize, unit: &str) -> String {
    format!("{}{unit}", locale.decimal(shown, places))
}

/// A value formatted for its property (`120 px`, `50%`, `45°`,
/// `#ff8800ff`); whole numbers drop the decimal.
#[must_use]
#[allow(clippy::cast_precision_loss)] // Display only.
pub fn format_value(locale: Locale, property: Property, value: Value) -> String {
    match value {
        Value::Scalar(raw) => {
            let (unit, divisor) = property_unit(property);
            let shown = raw as f64 / divisor;
            // One decimal unless the value is (nearly) whole.
            let places = usize::from((shown - shown.round()).abs() >= 0.05);
            format_number(locale, shown, places, unit)
        }
        Value::Color(color) => format!(
            "#{:02x}{:02x}{:02x}{:02x}",
            color.r, color.g, color.b, color.a
        ),
    }
}

/// Display name of a preset.
#[must_use]
pub fn preset_name(locale: Locale, preset: AnimationPreset) -> &'static str {
    locale.text(match preset {
        AnimationPreset::Fade => Msg::TransitionFade,
        AnimationPreset::SlideFromLeft => Msg::AnimPresetSlideFromLeft,
        AnimationPreset::SlideFromRight => Msg::AnimPresetSlideFromRight,
        AnimationPreset::SlideFromTop => Msg::AnimPresetSlideFromTop,
        AnimationPreset::SlideFromBottom => Msg::AnimPresetSlideFromBottom,
        AnimationPreset::Pop => Msg::AnimPresetPop,
        AnimationPreset::Wipe => Msg::TransitionWipe,
        AnimationPreset::Drop => Msg::AnimPresetDrop,
    })
}

/// Hover explanation of a preset.
#[must_use]
pub fn preset_hover(locale: Locale, preset: AnimationPreset) -> &'static str {
    locale.text(match preset {
        AnimationPreset::Fade => Msg::AnimPresetFadeHover,
        AnimationPreset::SlideFromLeft => Msg::AnimPresetSlideFromLeftHover,
        AnimationPreset::SlideFromRight => Msg::AnimPresetSlideFromRightHover,
        AnimationPreset::SlideFromTop => Msg::AnimPresetSlideFromTopHover,
        AnimationPreset::SlideFromBottom => Msg::AnimPresetSlideFromBottomHover,
        AnimationPreset::Pop => Msg::AnimPresetPopHover,
        AnimationPreset::Wipe => Msg::AnimPresetWipeHover,
        AnimationPreset::Drop => Msg::AnimPresetDropHover,
    })
}

/// Display name of an easing.
#[must_use]
pub fn easing_name(locale: Locale, easing: Easing) -> &'static str {
    match EasingPreset::from_easing(easing) {
        Some(preset) => easing_preset_name(locale, preset),
        None if matches!(easing, Easing::Step) => locale.text(Msg::AnimEaseStep),
        None => locale.text(Msg::AnimEaseCustom),
    }
}

/// Display name of a named easing.
#[must_use]
pub fn easing_preset_name(locale: Locale, preset: EasingPreset) -> &'static str {
    locale.text(match preset {
        EasingPreset::Linear => Msg::AnimEaseLinear,
        EasingPreset::Hold => Msg::AnimEaseHold,
        EasingPreset::QuadIn => Msg::AnimEaseGentleIn,
        EasingPreset::QuadOut => Msg::AnimEaseGentleOut,
        EasingPreset::QuadInOut => Msg::AnimEaseGentleInOut,
        EasingPreset::CubicIn => Msg::AnimEaseStrongIn,
        EasingPreset::CubicOut => Msg::AnimEaseStrongOut,
        EasingPreset::CubicInOut => Msg::AnimEaseStrongInOut,
        EasingPreset::SineIn => Msg::AnimEaseSoftIn,
        EasingPreset::SineOut => Msg::AnimEaseSoftOut,
        EasingPreset::SineInOut => Msg::AnimEaseSoftInOut,
        EasingPreset::BackIn => Msg::AnimEaseAnticipate,
        EasingPreset::BackOut => Msg::AnimEaseOvershoot,
        EasingPreset::BackInOut => Msg::AnimEaseAnticipateOvershoot,
        EasingPreset::BounceIn => Msg::AnimEaseBounceIn,
        EasingPreset::BounceOut => Msg::AnimEaseBounceOut,
        EasingPreset::BounceInOut => Msg::AnimEaseBounceInOut,
    })
}

/// Hover explanation of a named easing.
#[must_use]
pub fn easing_preset_hover(locale: Locale, preset: EasingPreset) -> &'static str {
    locale.text(match preset {
        EasingPreset::Linear => Msg::AnimEaseLinearHover,
        EasingPreset::Hold => Msg::AnimEaseHoldHover,
        EasingPreset::QuadIn | EasingPreset::CubicIn | EasingPreset::SineIn => Msg::AnimEaseInHover,
        EasingPreset::QuadOut | EasingPreset::CubicOut | EasingPreset::SineOut => {
            Msg::AnimEaseOutHover
        }
        EasingPreset::QuadInOut | EasingPreset::CubicInOut | EasingPreset::SineInOut => {
            Msg::AnimEaseInOutHover
        }
        EasingPreset::BackIn => Msg::AnimEaseAnticipateHover,
        EasingPreset::BackOut => Msg::AnimEaseOvershootHover,
        EasingPreset::BackInOut => Msg::AnimEaseAnticipateOvershootHover,
        EasingPreset::BounceIn => Msg::AnimEaseBounceInHover,
        EasingPreset::BounceOut => Msg::AnimEaseBounceOutHover,
        EasingPreset::BounceInOut => Msg::AnimEaseBounceInOutHover,
    })
}

/// Display name of a custom curve's starting shape.
#[must_use]
pub fn curve_preset_name(locale: Locale, preset: CurvePreset) -> &'static str {
    locale.text(match preset {
        CurvePreset::EaseIn => Msg::AnimCurvePresetEaseIn,
        CurvePreset::EaseOut => Msg::AnimCurvePresetEaseOut,
        CurvePreset::EaseInOut => Msg::AnimCurvePresetEaseInOut,
        CurvePreset::Back => Msg::AnimCurvePresetBack,
        CurvePreset::Anticipate => Msg::AnimEaseAnticipate,
    })
}

/// What a custom curve's starting shape does.
#[must_use]
pub fn curve_preset_hover(locale: Locale, preset: CurvePreset) -> &'static str {
    locale.text(match preset {
        CurvePreset::EaseIn => Msg::AnimEaseInHover,
        CurvePreset::EaseOut => Msg::AnimEaseOutHover,
        CurvePreset::EaseInOut => Msg::AnimEaseInOutHover,
        CurvePreset::Back => Msg::AnimEaseOvershootHover,
        CurvePreset::Anticipate => Msg::AnimEaseAnticipateHover,
    })
}

/// Hover help of one of a custom curve's four numbers.
#[must_use]
pub fn curve_control_hover(locale: Locale, control: CurveControl) -> &'static str {
    locale.text(match control {
        CurveControl::X1 => Msg::AnimCurveX1Hover,
        CurveControl::Y1 => Msg::AnimCurveY1Hover,
        CurveControl::X2 => Msg::AnimCurveX2Hover,
        CurveControl::Y2 => Msg::AnimCurveY2Hover,
    })
}

/// The glyph a key is drawn with and what it means.
#[must_use]
pub fn key_shape_name(locale: Locale, easing: Easing) -> &'static str {
    locale.text(match easing {
        Easing::Linear => Msg::AnimShapeLinear,
        Easing::Hold | Easing::Step => Msg::AnimShapeHold,
        _ => Msg::AnimShapeEased,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fm_ui_model::i18n::Language;

    const ES: Locale = Locale::new(Language::Spanish);

    #[test]
    fn core_errors_read_in_the_console_language() {
        assert_eq!(
            error_message(Locale::ENGLISH, "anim-editor-empty-clipboard"),
            Some("Nothing copied yet: copy keys first")
        );
        assert_eq!(error_message(ES, "anim-editor-something-new"), None);
        let error = AnimEditorError::<u32>::RetimeCollision {
            track: fm_animation::TrackKey::new(1, Phase::In, Property::Opacity),
            min_len: 7,
        };
        assert!(
            describe_error(Locale::ENGLISH, &error).ends_with("Shortest length that fits: 7 fr.")
        );
        assert!(describe_error(ES, &error).starts_with("Demasiado corto"));
        let error = AnimEditorError::<u32>::LimitExceeded(AnimLimit::ClipKeys { max: 10_000 });
        assert!(describe_error(ES, &error).ends_with("como máximo 10.000 claves en total."));
    }

    #[test]
    fn values_read_in_operator_units_and_locale_numbers() {
        let en = Locale::ENGLISH;
        assert_eq!(
            format_value(en, Property::ScaleX, Value::Scalar(5_000)),
            "50%"
        );
        assert_eq!(
            format_value(en, Property::Rotation, Value::Scalar(45_000)),
            "45°"
        );
        assert_eq!(
            format_value(en, Property::PositionX, Value::Scalar(-12)),
            "-12 px"
        );
        assert_eq!(
            format_value(en, Property::Opacity, Value::Scalar(255)),
            "100%"
        );
        assert_eq!(
            format_value(en, Property::Rotation, Value::Scalar(12_500)),
            "12.5°"
        );
        assert_eq!(
            format_value(ES, Property::Rotation, Value::Scalar(12_500)),
            "12,5°"
        );
        for preset in EasingPreset::ALL {
            assert_ne!(easing_preset_name(ES, preset), "");
            assert_ne!(easing_preset_hover(ES, preset), "");
        }
        assert_eq!(property_name(ES, Property::Color), "Color");
        assert_eq!(preset_name(ES, AnimationPreset::Fade), "Fundido");
    }
}
