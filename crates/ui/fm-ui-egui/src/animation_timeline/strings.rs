//! English text for the animation timeline: error messages keyed by
//! [`AnimEditorError::msg_key`], and names plus hover explanations for
//! properties, presets, and easings.
//!
//! TODO(l10n): these strings are a local English table. The shared catalog
//! (`fm_ui_model::i18n::Msg`) lives in `fm-ui-model`; move them there (with
//! Spanish translations) once the editor ships in Studio.

use fm_animation::{AnimationPreset, Easing, EasingPreset, Phase, Property, Value};
use fm_ui_model::animation_editor::{AnimEditorError, AnimLimit};

/// The English sentence for a core error id, or `None` for an unknown id.
#[must_use]
pub fn error_message(msg_key: &str) -> Option<&'static str> {
    Some(match msg_key {
        "anim-editor-retime-collision" => {
            "Too short: two keys on one property would land on the same frame"
        }
        "anim-editor-key-collision" => "Another key already sits on that frame",
        "anim-editor-limit-exceeded" => "That would exceed the animation's size limit",
        "anim-editor-key-outside-phase" => "A key would fall after the end of the phase",
        "anim-editor-invalid-value" => "That value is out of range for the property",
        "anim-editor-unknown-key" => "That key no longer exists",
        "anim-editor-unknown-target" => "That element is no longer in the design",
        "anim-editor-zero-span" => {
            "The selected keys are all on one frame, so there is nothing to stretch"
        }
        "anim-editor-empty-selection" => "Select one or more keys first",
        "anim-editor-empty-clipboard" => "Nothing copied yet: copy keys first",
        "anim-editor-no-drag" => "No drag is in progress",
        "anim-editor-drag-active" => "Finish the current drag first",
        "anim-editor-nothing-to-undo" => "Nothing to undo",
        "anim-editor-nothing-to-redo" => "Nothing to redo",
        "anim-editor-invalid-clip" => "The animation is inconsistent; reload it",
        _ => return None,
    })
}

/// The full inline message for `error`, with the detail an operator can act
/// on (the shortest length that fits, the colliding frame, the limit).
#[must_use]
pub fn describe_error<T>(error: &AnimEditorError<T>) -> String {
    let base = error_message(error.msg_key()).unwrap_or("The edit was refused");
    match error {
        AnimEditorError::RetimeCollision { min_len, .. } => {
            format!("{base}. Shortest length that fits: {min_len} fr.")
        }
        AnimEditorError::KeyCollision { frame, .. }
        | AnimEditorError::KeyOutsidePhase { frame, .. } => format!("{base} (frame {frame})."),
        AnimEditorError::LimitExceeded(limit) => match limit {
            AnimLimit::PhaseFrames { max } => format!("{base}: at most {max} frames per phase."),
            AnimLimit::Tracks { max } => format!("{base}: at most {max} animated properties."),
            AnimLimit::TrackKeys { max } => format!("{base}: at most {max} keys per property."),
            AnimLimit::ClipKeys { max } => format!("{base}: at most {max} keys in total."),
        },
        _ => format!("{base}."),
    }
}

/// Tab label of a phase.
#[must_use]
pub const fn phase_label(phase: Phase) -> &'static str {
    match phase {
        Phase::In => "IN",
        Phase::Out => "OUT",
    }
}

/// Hover explanation of a phase tab.
#[must_use]
pub const fn phase_hover(phase: Phase) -> &'static str {
    match phase {
        Phase::In => {
            "IN: the animation played when the graphic is taken to air. \
             Its last frame is held for as long as the graphic stays on air."
        }
        Phase::Out => "OUT: the animation played when the graphic is taken off air.",
    }
}

/// Hover explanation of the (non-editable) hold segment between the phases.
pub const HOLD_HOVER: &str = "HOLD: while on air the graphic rests on the last frame of IN. \
     There is nothing to key here; animate IN and OUT instead.";

/// Display name of a property.
#[must_use]
pub const fn property_name(property: Property) -> &'static str {
    match property {
        Property::PositionX => "Position X",
        Property::PositionY => "Position Y",
        Property::ScaleX => "Scale X",
        Property::ScaleY => "Scale Y",
        Property::Rotation => "Rotation",
        Property::Opacity => "Opacity",
        Property::CropLeft => "Crop left",
        Property::CropTop => "Crop top",
        Property::CropRight => "Crop right",
        Property::CropBottom => "Crop bottom",
        Property::Color => "Colour",
    }
}

/// Hover explanation of a property row.
#[must_use]
pub const fn property_hover(property: Property) -> &'static str {
    match property {
        Property::PositionX => "Horizontal offset in pixels, added to the element's position",
        Property::PositionY => "Vertical offset in pixels, added to the element's position",
        Property::ScaleX => "Width scale; 100% is the designed size",
        Property::ScaleY => "Height scale; 100% is the designed size",
        Property::Rotation => "Rotation in degrees, added to the designed rotation",
        Property::Opacity => "Opacity multiplied into the designed opacity; 100% leaves it as is",
        Property::CropLeft => "Pixels cropped from the left edge",
        Property::CropTop => "Pixels cropped from the top edge",
        Property::CropRight => "Pixels cropped from the right edge",
        Property::CropBottom => "Pixels cropped from the bottom edge",
        Property::Color => "Colour that replaces the designed colour",
    }
}

/// Unit suffix and divisor turning a stored scalar into the shown number.
#[must_use]
pub const fn property_unit(property: Property) -> (&'static str, f64) {
    match property {
        Property::ScaleX | Property::ScaleY => ("%", 100.0),
        Property::Rotation => ("°", 1_000.0),
        Property::Opacity => ("%", 2.55),
        Property::Color => ("", 1.0),
        _ => (" px", 1.0),
    }
}

/// A value formatted for its property (`120 px`, `50%`, `45°`, `#ff8800ff`).
#[must_use]
#[allow(clippy::cast_precision_loss)] // Display only.
pub fn format_value(property: Property, value: Value) -> String {
    match value {
        Value::Scalar(raw) => {
            let (unit, divisor) = property_unit(property);
            let shown = raw as f64 / divisor;
            if (shown - shown.round()).abs() < 0.05 {
                format!("{shown:.0}{unit}")
            } else {
                format!("{shown:.1}{unit}")
            }
        }
        Value::Color(color) => format!(
            "#{:02x}{:02x}{:02x}{:02x}",
            color.r, color.g, color.b, color.a
        ),
    }
}

/// Display name of a preset.
#[must_use]
pub const fn preset_name(preset: AnimationPreset) -> &'static str {
    match preset {
        AnimationPreset::Fade => "Fade",
        AnimationPreset::SlideFromLeft => "Slide from left",
        AnimationPreset::SlideFromRight => "Slide from right",
        AnimationPreset::SlideFromTop => "Slide from top",
        AnimationPreset::SlideFromBottom => "Slide from bottom",
        AnimationPreset::Pop => "Pop",
        AnimationPreset::Wipe => "Wipe",
        AnimationPreset::Drop => "Drop",
    }
}

/// Hover explanation of a preset.
#[must_use]
pub const fn preset_hover(preset: AnimationPreset) -> &'static str {
    match preset {
        AnimationPreset::Fade => "Fades the element's opacity in (or out)",
        AnimationPreset::SlideFromLeft => "Slides the element in from beyond the left edge",
        AnimationPreset::SlideFromRight => "Slides the element in from beyond the right edge",
        AnimationPreset::SlideFromTop => "Slides the element in from above the frame",
        AnimationPreset::SlideFromBottom => "Slides the element in from below the frame",
        AnimationPreset::Pop => "Scales the element up from nothing with a small overshoot",
        AnimationPreset::Wipe => "Reveals the element left to right by animating its crop",
        AnimationPreset::Drop => "Drops the element from above with a bounce",
    }
}

/// Display name of an easing.
#[must_use]
pub fn easing_name(easing: Easing) -> &'static str {
    match EasingPreset::from_easing(easing) {
        Some(preset) => easing_preset_name(preset),
        None if matches!(easing, Easing::Step) => "Step",
        None => "Custom curve",
    }
}

/// Display name of a named easing.
#[must_use]
pub const fn easing_preset_name(preset: EasingPreset) -> &'static str {
    match preset {
        EasingPreset::Linear => "Linear",
        EasingPreset::Hold => "Hold",
        EasingPreset::QuadIn => "Gentle in",
        EasingPreset::QuadOut => "Gentle out",
        EasingPreset::QuadInOut => "Gentle in-out",
        EasingPreset::CubicIn => "Strong in",
        EasingPreset::CubicOut => "Strong out",
        EasingPreset::CubicInOut => "Strong in-out",
        EasingPreset::SineIn => "Soft in",
        EasingPreset::SineOut => "Soft out",
        EasingPreset::SineInOut => "Soft in-out",
        EasingPreset::BackIn => "Anticipate",
        EasingPreset::BackOut => "Overshoot",
        EasingPreset::BackInOut => "Anticipate + overshoot",
        EasingPreset::BounceIn => "Bounce in",
        EasingPreset::BounceOut => "Bounce out",
        EasingPreset::BounceInOut => "Bounce in-out",
    }
}

/// Hover explanation of a named easing.
#[must_use]
pub const fn easing_preset_hover(preset: EasingPreset) -> &'static str {
    match preset {
        EasingPreset::Linear => "Constant speed from this key to the next",
        EasingPreset::Hold => "Keeps this value, then jumps to the next key's value on its frame",
        EasingPreset::QuadIn | EasingPreset::CubicIn | EasingPreset::SineIn => {
            "Starts slowly and speeds up into the next key"
        }
        EasingPreset::QuadOut | EasingPreset::CubicOut | EasingPreset::SineOut => {
            "Starts fast and settles gently into the next key (the most natural entrance)"
        }
        EasingPreset::QuadInOut | EasingPreset::CubicInOut | EasingPreset::SineInOut => {
            "Speeds up then slows down: smooth at both keys"
        }
        EasingPreset::BackIn => "Pulls back slightly before moving towards the next key",
        EasingPreset::BackOut => "Passes the next key's value slightly, then settles back",
        EasingPreset::BackInOut => "Pulls back first and overshoots at the end",
        EasingPreset::BounceIn => "Bounces a few times before leaving",
        EasingPreset::BounceOut => "Lands on the next key's value with a few bounces",
        EasingPreset::BounceInOut => "Bounces at both ends",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEYS: [&str; 15] = [
        "anim-editor-retime-collision",
        "anim-editor-key-collision",
        "anim-editor-limit-exceeded",
        "anim-editor-key-outside-phase",
        "anim-editor-invalid-value",
        "anim-editor-unknown-key",
        "anim-editor-unknown-target",
        "anim-editor-zero-span",
        "anim-editor-empty-selection",
        "anim-editor-empty-clipboard",
        "anim-editor-no-drag",
        "anim-editor-drag-active",
        "anim-editor-nothing-to-undo",
        "anim-editor-nothing-to-redo",
        "anim-editor-invalid-clip",
    ];

    #[test]
    fn every_core_error_id_has_an_english_message() {
        for key in KEYS {
            assert!(
                error_message(key).is_some_and(|text| !text.is_empty()),
                "{key}"
            );
        }
        assert_eq!(error_message("anim-editor-something-new"), None);
        let error = AnimEditorError::<u32>::RetimeCollision {
            track: fm_animation::TrackKey::new(1, Phase::In, Property::Opacity),
            min_len: 7,
        };
        assert!(describe_error(&error).ends_with("Shortest length that fits: 7 fr."));
    }

    #[test]
    fn values_read_in_operator_units() {
        assert_eq!(format_value(Property::ScaleX, Value::Scalar(5_000)), "50%");
        assert_eq!(
            format_value(Property::Rotation, Value::Scalar(45_000)),
            "45°"
        );
        assert_eq!(
            format_value(Property::PositionX, Value::Scalar(-12)),
            "-12 px"
        );
        assert_eq!(format_value(Property::Opacity, Value::Scalar(255)), "100%");
        for preset in EasingPreset::ALL {
            assert_ne!(easing_preset_name(preset), "");
            assert_ne!(easing_preset_hover(preset), "");
        }
    }
}
