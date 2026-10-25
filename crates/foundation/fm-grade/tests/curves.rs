// The assertions compare exact reference values.
#![allow(clippy::float_cmp)]

use fm_grade::{MonotoneCubic, PeriodicCatmullRom};

#[allow(clippy::cast_precision_loss)]
fn sweep(samples: usize) -> impl Iterator<Item = f32> {
    (0..=samples).map(move |index| index as f32 / samples as f32)
}

#[test]
fn monotone_cubic_passes_every_knot_exactly() {
    let knots = [(0.0, 0.0), (0.2, 0.5), (0.45, 0.55), (0.8, 0.9), (1.0, 1.0)];
    let curve = MonotoneCubic::new(&knots).unwrap();
    for (x, y) in knots {
        assert_eq!(curve.evaluate(x).to_bits(), f32::to_bits(y), "knot {x}");
    }
}

#[test]
fn monotone_cubic_is_monotone_for_monotone_data() {
    let data: [&[(f32, f32)]; 3] = [
        &[(0.0, 0.0), (0.1, 0.8), (0.2, 0.81), (1.0, 1.0)],
        &[(0.0, 0.0), (0.5, 0.0), (0.51, 1.0), (1.0, 1.0)],
        &[(0.0, 1.0), (0.3, 0.9), (0.35, 0.1), (1.0, 0.0)],
    ];
    for knots in data {
        let curve = MonotoneCubic::new(knots).unwrap();
        let increasing = knots[knots.len() - 1].1 >= knots[0].1;
        let mut previous = curve.evaluate(0.0);
        for x in sweep(20_000) {
            let value = curve.evaluate(x);
            if increasing {
                assert!(value >= previous, "{knots:?} decreases at {x}");
            } else {
                assert!(value <= previous, "{knots:?} increases at {x}");
            }
            previous = value;
        }
    }
}

#[test]
fn monotone_cubic_never_overshoots_neighbouring_knots() {
    // Step-like and non-monotone data that would ring under a natural spline.
    let knots = [
        (0.0, 0.0),
        (0.2, 0.0),
        (0.25, 1.0),
        (0.6, 1.0),
        (0.7, 0.3),
        (0.9, 0.35),
        (1.0, 0.0),
    ];
    let curve = MonotoneCubic::new(&knots).unwrap();
    for x in sweep(20_000) {
        let value = curve.evaluate(x);
        let segment = knots
            .windows(2)
            .find(|pair| pair[0].0 <= x && x <= pair[1].0)
            .unwrap();
        let low = segment[0].1.min(segment[1].1);
        let high = segment[0].1.max(segment[1].1);
        assert!(
            (low..=high).contains(&value),
            "{value} outside [{low}, {high}] at {x}"
        );
    }
    // Flat segments stay exactly flat.
    assert_eq!(curve.evaluate(0.1), 0.0);
    assert_eq!(curve.evaluate(0.4), 1.0);
}

#[test]
fn monotone_cubic_endpoints_are_fixed_and_extrapolation_is_constant() {
    let curve = MonotoneCubic::new(&[(0.0, 0.06), (0.5, 0.4), (1.0, 0.94)]).unwrap();
    assert_eq!(curve.evaluate(0.0), 0.06);
    assert_eq!(curve.evaluate(1.0), 0.94);
    assert_eq!(curve.evaluate(-3.0), 0.06);
    assert_eq!(curve.evaluate(7.0), 0.94);
    assert_eq!(curve.evaluate(f32::NAN), 0.06);

    let inner = MonotoneCubic::new(&[(0.3, -0.5), (0.6, 0.25)]).unwrap();
    assert_eq!(inner.evaluate(0.0), -0.5);
    assert_eq!(inner.evaluate(1.0), 0.25);

    let constant = MonotoneCubic::new(&[(0.4, 0.7)]).unwrap();
    assert_eq!(constant.evaluate(0.0), 0.7);
    assert_eq!(constant.evaluate(1.0), 0.7);
}

#[test]
fn two_point_monotone_cubic_is_linear() {
    let curve = MonotoneCubic::new(&[(0.0, 0.2), (1.0, 0.8)]).unwrap();
    for x in sweep(1000) {
        assert!((curve.evaluate(x) - (0.2 + 0.6 * x)).abs() < 1e-6);
    }
}

#[test]
fn fritsch_carlson_limits_steep_tangents() {
    // α² + β² > 9 on the first segment: unconstrained tangents would overshoot
    // 1.0 on the way into the plateau.
    let curve = MonotoneCubic::new(&[(0.0, 0.0), (0.1, 0.9), (0.2, 1.0), (1.0, 1.0)]).unwrap();
    for x in sweep(10_000) {
        assert!(curve.evaluate(x) <= 1.0);
    }
}

#[test]
fn invalid_knots_are_rejected() {
    assert!(MonotoneCubic::new(&[]).is_none());
    assert!(MonotoneCubic::new(&[(0.0, 0.0), (0.0, 1.0)]).is_none());
    assert!(MonotoneCubic::new(&[(0.5, 0.0), (0.1, 1.0)]).is_none());
    assert!(MonotoneCubic::new(&[(0.0, f32::NAN), (1.0, 1.0)]).is_none());
    assert!(PeriodicCatmullRom::new(&[], 1.0).is_none());
    assert!(PeriodicCatmullRom::new(&[(0.0, 0.0)], 0.0).is_none());
    assert!(PeriodicCatmullRom::new(&[(1.0, 0.0)], 1.0).is_none());
    assert!(PeriodicCatmullRom::new(&[(0.2, 0.0), (0.2, 1.0)], 1.0).is_none());
    assert!(PeriodicCatmullRom::new(&[(0.2, f32::INFINITY)], 1.0).is_none());
}

#[test]
fn periodic_catmull_rom_passes_knots_and_wraps_continuously() {
    let knots = [(0.1, 0.5), (0.3, -0.2), (0.55, 0.8), (0.9, 0.0)];
    let curve = PeriodicCatmullRom::new(&knots, 1.0).unwrap();
    for (x, y) in knots {
        assert!((curve.evaluate(x) - y).abs() < 1e-6, "knot {x}");
    }
    // Value continuity across the period boundary and periodicity.
    let epsilon = 1e-4;
    assert!((curve.evaluate(1.0 - epsilon) - curve.evaluate(epsilon)).abs() < 1e-3);
    assert!((curve.evaluate(0.0) - curve.evaluate(1.0)).abs() < 1e-6);
    assert!((curve.evaluate(0.37) - curve.evaluate(2.37)).abs() < 1e-5);
    assert!((curve.evaluate(0.37) - curve.evaluate(-0.63)).abs() < 1e-5);

    // Derivative continuity (C¹) at the boundary and at a knot: one-sided
    // differences converge to the same slope.
    let step = 1e-4;
    let left = (curve.evaluate(1.0) - curve.evaluate(1.0 - step)) / step;
    let right = (curve.evaluate(step) - curve.evaluate(0.0)) / step;
    assert!((left - right).abs() < 0.05, "{left} vs {right}");
    let before = (curve.evaluate(0.3) - curve.evaluate(0.3 - step)) / step;
    let after = (curve.evaluate(0.3 + step) - curve.evaluate(0.3)) / step;
    assert!((before - after).abs() < 0.05, "{before} vs {after}");

    // Knots straddling the boundary interpolate through it.
    let wrap = PeriodicCatmullRom::new(&[(0.05, 1.0), (0.95, 1.0)], 1.0).unwrap();
    assert!((wrap.evaluate(0.0) - 1.0).abs() < 1e-6);
    assert!((wrap.evaluate(0.99) - 1.0).abs() < 1e-6);
}

#[test]
fn single_knot_periodic_curve_is_constant() {
    let curve = PeriodicCatmullRom::new(&[(0.4, 0.3)], 1.0).unwrap();
    for x in sweep(100) {
        assert_eq!(curve.evaluate(x), 0.3);
    }
    assert_eq!(curve.evaluate(f32::NAN), 0.3);
}

#[test]
fn periodic_catmull_rom_non_finite_input_evaluates_as_the_first_knot() {
    let curve = PeriodicCatmullRom::new(&[(0.1, 0.5), (0.6, -0.25)], 1.0).unwrap();
    assert_eq!(curve.evaluate(f32::INFINITY), 0.5);
    assert_eq!(curve.evaluate(f32::NEG_INFINITY), 0.5);
    // `x - first` overflows to -inf for a finite `x`.
    let wide = PeriodicCatmullRom::new(&[(1.0e38, 0.5), (1.5e38, -0.25)], 2.0e38).unwrap();
    assert!(wide.evaluate(-3.0e38).is_finite());
}

#[test]
fn splines_never_evaluate_to_non_finite_values() {
    let sweep_finite = |evaluate: &dyn Fn(f32) -> f32, from: f32, to: f32| {
        for step in 0..=1000_u16 {
            let x = from + (to - from) * f32::from(step) / 1000.0;
            let y = evaluate(x);
            assert!(y.is_finite(), "{x} -> {y}");
        }
    };
    // Secant overflow.
    if let Some(curve) = MonotoneCubic::new(&[(0.0, -3.0e38), (1.0, 3.0e38)]) {
        sweep_finite(&|x| curve.evaluate(x), 0.0, 1.0);
    }
    // Finite secants whose tangent ratio overflows.
    if let Some(curve) =
        MonotoneCubic::new(&[(0.0, 0.0), (1.0e-6, 1.0e30), (1.0e30, 1.0e30 + 1.0e23)])
    {
        sweep_finite(&|x| curve.evaluate(x), 0.0, 1.0e-6);
        sweep_finite(&|x| curve.evaluate(x), 1.0e-6, 1.0e30);
    }
    // Catmull–Rom tangent overflow.
    if let Some(curve) = PeriodicCatmullRom::new(&[(0.0, -3.0e38), (0.1, 0.0), (0.2, 3.0e38)], 1.0)
    {
        sweep_finite(&|x| curve.evaluate(x), 0.0, 1.0);
    }
    // The wrap-around segment ends at `first + period`, which overflows.
    if let Some(curve) = PeriodicCatmullRom::new(&[(1.0e38, 0.0), (2.0e38, 1.0)], 3.3e38) {
        sweep_finite(&|x| curve.evaluate(x), 0.0, 3.3e38);
    }
}
