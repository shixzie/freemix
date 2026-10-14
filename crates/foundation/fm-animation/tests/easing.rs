#![allow(clippy::cast_precision_loss, clippy::manual_midpoint)]

use fm_animation::{EASE_ONE, Easing, EasingPreset};
use std::collections::BTreeSet;
use std::f64::consts::PI;

const ONE: i32 = 1 << 16;

fn reference(preset: EasingPreset, x: f64) -> Option<f64> {
    let c1 = 1.701_58;
    let c2 = c1 * 1.525;
    let c3 = c1 + 1.0;
    let bounce_out = |x: f64| {
        let (n1, d1) = (7.5625, 2.75);
        if x < 1.0 / d1 {
            n1 * x * x
        } else if x < 2.0 / d1 {
            let x = x - 1.5 / d1;
            n1 * x * x + 0.75
        } else if x < 2.5 / d1 {
            let x = x - 2.25 / d1;
            n1 * x * x + 0.9375
        } else {
            let x = x - 2.625 / d1;
            n1 * x * x + 0.984_375
        }
    };
    Some(match preset {
        EasingPreset::SineIn => 1.0 - (x * PI / 2.0).cos(),
        EasingPreset::SineOut => (x * PI / 2.0).sin(),
        EasingPreset::SineInOut => -((PI * x).cos() - 1.0) / 2.0,
        EasingPreset::BackIn => c3 * x * x * x - c1 * x * x,
        EasingPreset::BackOut => 1.0 + c3 * (x - 1.0).powi(3) + c1 * (x - 1.0).powi(2),
        EasingPreset::BackInOut => {
            if x < 0.5 {
                ((2.0 * x).powi(2) * ((c2 + 1.0) * 2.0 * x - c2)) / 2.0
            } else {
                ((2.0 * x - 2.0).powi(2) * ((c2 + 1.0) * (x * 2.0 - 2.0) + c2) + 2.0) / 2.0
            }
        }
        EasingPreset::BounceOut => bounce_out(x),
        EasingPreset::BounceIn => 1.0 - bounce_out(1.0 - x),
        EasingPreset::BounceInOut => {
            if x < 0.5 {
                (1.0 - bounce_out(1.0 - 2.0 * x)) / 2.0
            } else {
                (1.0 + bounce_out(2.0 * x - 1.0)) / 2.0
            }
        }
        _ => return None,
    })
}

#[test]
fn every_preset_hits_exact_q16_endpoints_and_clamps_progress() {
    for preset in EasingPreset::ALL {
        let easing = preset.easing();
        assert_eq!(easing.apply(0), 0, "{preset:?} at 0");
        assert_eq!(easing.apply(EASE_ONE), ONE, "{preset:?} at 1");
        assert_eq!(easing.apply(EASE_ONE * 3), ONE, "{preset:?} clamps");
    }
}

#[test]
fn preset_ids_are_unique_stable_and_round_trip() {
    let ids: BTreeSet<&str> = EasingPreset::ALL.iter().map(|preset| preset.id()).collect();
    assert_eq!(ids.len(), EasingPreset::ALL.len());
    for preset in EasingPreset::ALL {
        assert_eq!(EasingPreset::from_id(preset.id()), Some(preset));
        assert_eq!(EasingPreset::from_easing(preset.easing()), Some(preset));
    }
    assert_eq!(
        EasingPreset::from_id("back-out"),
        Some(EasingPreset::BackOut)
    );
    assert_eq!(
        EasingPreset::from_id("bounce-in-out"),
        Some(EasingPreset::BounceInOut)
    );
    assert_eq!(EasingPreset::from_id("elastic"), None);
    assert_eq!(
        EasingPreset::from_easing(Easing::Step),
        Some(EasingPreset::Hold)
    );
}

#[test]
fn hold_is_step() {
    for progress in (0..=EASE_ONE).step_by(7).chain([EASE_ONE - 1, EASE_ONE]) {
        assert_eq!(
            Easing::Hold.apply(progress),
            Easing::Step.apply(progress),
            "{progress}"
        );
    }
    assert_eq!(Easing::Hold.apply(EASE_ONE - 1), 0);
}

#[test]
fn new_curves_are_within_documented_error_of_the_real_curve() {
    let one = f64::from(EASE_ONE);
    for preset in EasingPreset::ALL {
        for progress in 0..=EASE_ONE {
            let Some(real) = reference(preset, f64::from(progress) / one) else {
                break;
            };
            let error = (f64::from(preset.easing().apply(progress)) - real * one).abs();
            // One rounding (2^-17 = 0.5 ulp) plus f64 reference noise.
            assert!(error <= 0.5 + 1e-6, "{preset:?} at {progress}: {error}");
        }
    }
}

#[test]
fn sine_curves_are_monotonic_and_symmetric() {
    for easing in [Easing::SineIn, Easing::SineOut, Easing::SineInOut] {
        let mut previous = 0;
        for progress in 0..=EASE_ONE {
            let value = easing.apply(progress);
            assert!(value >= previous && value <= ONE, "{easing:?} {progress}");
            previous = value;
        }
    }
    for progress in (0..=EASE_ONE).step_by(13) {
        let mirror = EASE_ONE - progress;
        assert_eq!(
            Easing::SineOut.apply(progress),
            ONE - Easing::SineIn.apply(mirror)
        );
        assert_eq!(
            Easing::SineInOut.apply(progress) + Easing::SineInOut.apply(mirror),
            ONE
        );
        assert_eq!(
            Easing::BackInOut.apply(progress) + Easing::BackInOut.apply(mirror),
            ONE
        );
        assert_eq!(
            Easing::BounceInOut.apply(progress) + Easing::BounceInOut.apply(mirror),
            ONE
        );
    }
    assert_eq!(Easing::SineInOut.apply(EASE_ONE / 2), ONE / 2);
}

#[test]
fn back_overshoots_inside_minus_one_to_two() {
    let low = -ONE;
    let high = 2 * ONE;
    for easing in [Easing::BackIn, Easing::BackOut, Easing::BackInOut] {
        let mut minimum = i32::MAX;
        let mut maximum = i32::MIN;
        for progress in 0..=EASE_ONE {
            let value = easing.apply(progress);
            assert!((low..=high).contains(&value), "{easing:?} {progress}");
            minimum = minimum.min(value);
            maximum = maximum.max(value);
        }
        match easing {
            Easing::BackIn => assert!(minimum < 0 && maximum == ONE),
            Easing::BackOut => assert!(maximum > ONE && minimum == 0),
            _ => assert!(minimum < 0 && maximum > ONE),
        }
    }
    // BackIn's dip is -0.1000 (about -6554 in Q16).
    let dip = (0..=EASE_ONE)
        .map(|p| Easing::BackIn.apply(p))
        .min()
        .unwrap();
    assert!((-6_555..=-6_553).contains(&dip), "{dip}");
}

#[test]
fn bounce_is_continuous_at_joins_and_touches_one_there() {
    // The steepest bounce slope is 2 * d1 = 5.5, so one Q16 progress step
    // can change the value by at most 7 ulp including rounding.
    for easing in [Easing::BounceIn, Easing::BounceOut, Easing::BounceInOut] {
        for progress in 1..=EASE_ONE {
            let step = (easing.apply(progress) - easing.apply(progress - 1)).abs();
            assert!(step <= 7, "{easing:?} jumps {step} at {progress}");
        }
    }
    // Joins of BounceOut at x = 4/11, 8/11, 10/11 touch 1.0.
    for join in [4_u32, 8, 10] {
        let progress = (join * EASE_ONE).div_ceil(11);
        for at in [progress - 1, progress] {
            let value = Easing::BounceOut.apply(at);
            assert!((ONE - 7..=ONE).contains(&value), "join {join}: {value}");
        }
    }
    // (11/8 - 3/2)^2 + 3/4 = 0.765625 exactly.
    assert_eq!(Easing::BounceOut.apply(EASE_ONE / 2), 50_176);
}

#[test]
fn bezier_seeds_exist_for_every_preset_but_hold() {
    for preset in EasingPreset::ALL {
        let bezier = preset.to_bezier();
        if preset == EasingPreset::Hold {
            assert_eq!(bezier, None);
            continue;
        }
        let curve = Easing::CubicBezier(bezier.unwrap_or_else(|| panic!("{preset:?}")));
        assert_eq!(curve.apply(0), 0);
        assert_eq!(curve.apply(EASE_ONE), ONE);
    }
    // Linear's seed has x(t) == y(t): linear within one Q16 step.
    let linear = Easing::CubicBezier(EasingPreset::Linear.to_bezier().unwrap());
    for progress in 0..=EASE_ONE {
        let error = linear.apply(progress) - i32::try_from(progress).unwrap();
        assert!((0..=1).contains(&error), "{progress}: {error}");
    }
    // Back seeds overshoot like the real curve.
    let back = Easing::CubicBezier(EasingPreset::BackOut.to_bezier().unwrap());
    assert!((0..EASE_ONE).any(|p| back.apply(p) > ONE));
    // Seeding does not change the preset: Back stays exact until edited.
    assert_ne!(EasingPreset::BackOut.easing(), back);
}
