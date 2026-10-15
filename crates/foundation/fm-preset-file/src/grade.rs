//! The strict JSON form of one [`fm_grade::Grade`], shared by
//! `project.json` and `.fmgrade` files.
//!
//! ```text
//! {"enabled": true, "white_balance": {"temperature": 0, "tint": 0},
//!  "exposure_millistops": 0, "contrast": {"amount": 0, "pivot_bp": 5000},
//!  "saturation_permille": 1000,
//!  "wheels": {"lift": {"x": 0, "y": 0, "master": 0}, "gamma": ..., "gain": ..., "offset": ...},
//!  "curves": {"master": [{"x": 0, "y": 0}, ...], "red": [...], "green": [...], "blue": [...]},
//!  "hue_curves": {"hue_vs_hue": [{"position": 0, "value": 0}, ...],
//!                 "hue_vs_saturation": [...], "luma_vs_saturation": [...]},
//!  "lut": null | {"asset_uri": "asset://luts/1.cube", "strength_permille": 1000}}
//! ```
//!
//! Every stage is spelled out with its exact integers; bounds are checked by
//! [`fm_grade::Grade::validate`], not here.

use std::fmt::Write as _;

use fm_grade::{
    Contrast, Curve, CurvePoint, Grade, GradeLut, HueCurve, HueCurves, HuePoint, RgbCurves, Wheel,
    Wheels, WhiteBalance,
};

use crate::json::{Object, SyntaxError, Value, escape_string, parse_array, parse_optional};

/// Writes `grade`'s JSON object.
pub fn write_grade(output: &mut String, grade: &Grade) {
    let wb = grade.white_balance;
    write!(
        output,
        "{{\"enabled\": {}, \"white_balance\": {{\"temperature\": {}, \"tint\": {}}}, \
         \"exposure_millistops\": {}, \"contrast\": {{\"amount\": {}, \"pivot_bp\": {}}}, \
         \"saturation_permille\": {}, \"wheels\": {{",
        grade.enabled,
        wb.temperature,
        wb.tint,
        grade.exposure_millistops,
        grade.contrast.amount,
        grade.contrast.pivot_bp,
        grade.saturation_permille
    )
    .expect("writing to a string");
    let wheels = grade.wheels;
    for (index, (name, wheel)) in [
        ("lift", wheels.lift),
        ("gamma", wheels.gamma),
        ("gain", wheels.gain),
        ("offset", wheels.offset),
    ]
    .into_iter()
    .enumerate()
    {
        if index != 0 {
            output.push_str(", ");
        }
        write!(
            output,
            "\"{name}\": {{\"x\": {}, \"y\": {}, \"master\": {}}}",
            wheel.x, wheel.y, wheel.master
        )
        .expect("writing to a string");
    }
    output.push_str("}, \"curves\": {");
    write_curves(output, &grade.curves, &grade.hue_curves);
    output.push_str("}, \"lut\": ");
    match &grade.lut {
        Some(lut) => {
            output.push_str("{\"asset_uri\": \"");
            escape_string(output, &lut.asset_uri);
            write!(
                output,
                "\", \"strength_permille\": {}}}",
                lut.strength_permille
            )
            .expect("writing to a string");
        }
        None => output.push_str("null"),
    }
    output.push('}');
}

fn write_curves(output: &mut String, curves: &RgbCurves, hue: &HueCurves) {
    for (index, (name, curve)) in [
        ("master", &curves.master),
        ("red", &curves.red),
        ("green", &curves.green),
        ("blue", &curves.blue),
    ]
    .into_iter()
    .enumerate()
    {
        if index != 0 {
            output.push_str(", ");
        }
        write!(output, "\"{name}\": [").expect("writing to a string");
        for (point_index, point) in curve.points().iter().enumerate() {
            if point_index != 0 {
                output.push_str(", ");
            }
            write!(output, "{{\"x\": {}, \"y\": {}}}", point.x, point.y)
                .expect("writing to a string");
        }
        output.push(']');
    }
    output.push_str("}, \"hue_curves\": {");
    for (index, (name, curve)) in [
        ("hue_vs_hue", &hue.hue_vs_hue),
        ("hue_vs_saturation", &hue.hue_vs_saturation),
        ("luma_vs_saturation", &hue.luma_vs_saturation),
    ]
    .into_iter()
    .enumerate()
    {
        if index != 0 {
            output.push_str(", ");
        }
        write!(output, "\"{name}\": [").expect("writing to a string");
        for (point_index, point) in curve.points().iter().enumerate() {
            if point_index != 0 {
                output.push_str(", ");
            }
            write!(
                output,
                "{{\"position\": {}, \"value\": {}}}",
                point.position, point.value
            )
            .expect("writing to a string");
        }
        output.push(']');
    }
}

/// Parses one grade object (see the module documentation).
///
/// # Errors
///
/// [`SyntaxError`] for a missing, unknown, or mistyped field.
pub fn parse_grade(value: Value) -> Result<Grade, SyntaxError> {
    let mut object = Object::new(value, "grade")?;
    let enabled = object.boolean("enabled")?;
    let mut wb = Object::new(object.take("white_balance")?, "white balance")?;
    let white_balance = WhiteBalance::new(wb.i16("temperature")?, wb.i16("tint")?);
    wb.finish()?;
    let exposure_millistops = object.i16("exposure_millistops")?;
    let mut contrast = Object::new(object.take("contrast")?, "contrast")?;
    let contrast_value = Contrast::new(contrast.i16("amount")?, contrast.u16("pivot_bp")?);
    contrast.finish()?;
    let saturation_permille = object.u16("saturation_permille")?;
    let mut wheels = Object::new(object.take("wheels")?, "wheels")?;
    let wheels_value = Wheels {
        lift: parse_wheel(wheels.take("lift")?)?,
        gamma: parse_wheel(wheels.take("gamma")?)?,
        gain: parse_wheel(wheels.take("gain")?)?,
        offset: parse_wheel(wheels.take("offset")?)?,
    };
    wheels.finish()?;
    let mut curves = Object::new(object.take("curves")?, "curves")?;
    let curves_value = RgbCurves {
        master: parse_curve(curves.take("master")?)?,
        red: parse_curve(curves.take("red")?)?,
        green: parse_curve(curves.take("green")?)?,
        blue: parse_curve(curves.take("blue")?)?,
    };
    curves.finish()?;
    let mut hue = Object::new(object.take("hue_curves")?, "hue curves")?;
    let hue_curves = HueCurves {
        hue_vs_hue: parse_hue_curve(hue.take("hue_vs_hue")?)?,
        hue_vs_saturation: parse_hue_curve(hue.take("hue_vs_saturation")?)?,
        luma_vs_saturation: parse_hue_curve(hue.take("luma_vs_saturation")?)?,
    };
    hue.finish()?;
    let lut = parse_optional(object.take("lut")?, |value| {
        let mut lut = Object::new(value, "grade lut")?;
        let asset_uri = lut.string("asset_uri")?;
        let strength_permille = lut.u16("strength_permille")?;
        lut.finish()?;
        Ok::<_, SyntaxError>(GradeLut::new(asset_uri, strength_permille))
    })?;
    object.finish()?;
    Ok(Grade {
        enabled,
        white_balance,
        exposure_millistops,
        contrast: contrast_value,
        saturation_permille,
        wheels: wheels_value,
        curves: curves_value,
        hue_curves,
        lut,
    })
}

fn parse_wheel(value: Value) -> Result<Wheel, SyntaxError> {
    let mut object = Object::new(value, "wheel")?;
    let wheel = Wheel::new(object.i16("x")?, object.i16("y")?, object.i16("master")?);
    object.finish()?;
    Ok(wheel)
}

fn parse_curve(value: Value) -> Result<Curve, SyntaxError> {
    parse_array(value, "curve", |value| {
        let mut object = Object::new(value, "curve point")?;
        let point = CurvePoint::new(object.u16("x")?, object.u16("y")?);
        object.finish()?;
        Ok::<_, SyntaxError>(point)
    })
    .map(Curve::new)
}

fn parse_hue_curve(value: Value) -> Result<HueCurve, SyntaxError> {
    parse_array(value, "hue curve", |value| {
        let mut object = Object::new(value, "hue curve point")?;
        let point = HuePoint::new(object.u16("position")?, object.i16("value")?);
        object.finish()?;
        Ok::<_, SyntaxError>(point)
    })
    .map(HueCurve::new)
}
