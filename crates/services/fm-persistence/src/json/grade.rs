//! Strict `project.json` encoding of the grade library, imported LUTs, and
//! the per-input and program grades.
//!
//! ```text
//! "luts": [{"id": 1, "name": "Film", "kind": "3d", "size": 33}],
//! "grades": [{"id": 1, "name": "Warm", "grade": GRADE}],
//! "input_grades": [{"input": 7, "grade": SOURCE}],
//! "program_grade": null | SOURCE
//! ```
//!
//! `SOURCE` is `{"type": "library", "grade": 1}` or
//! `{"type": "inline", "grade": GRADE}`. `GRADE` spells out every stage of an
//! [`fm_grade::Grade`] with its exact integers; curve points are
//! `{"x", "y"}` and hue-curve points `{"position", "value"}` objects.

use std::{collections::BTreeMap, fmt::Write as _, num::NonZeroU32};

use fm_grade::{
    Contrast, CubeKind, Curve, CurvePoint, Grade, GradeLut, HueCurve, HueCurves, HuePoint,
    RgbCurves, Wheel, Wheels, WhiteBalance,
};
use fm_model::{GradeId, GradeSource, LutId, ProjectGrade, ProjectGrading, ProjectLut};
use fm_types::InputId;

use super::{
    DecodeError, Object, Value, escape_string, parse_array, parse_optional, syntax, unknown_enum,
};

pub(crate) fn write_grading(output: &mut String, grading: &ProjectGrading) {
    output.push_str(",\n    \"luts\": [");
    for (index, lut) in grading.luts.iter().enumerate() {
        if index != 0 {
            output.push_str(", ");
        }
        write!(output, "{{\"id\": {}, \"name\": \"", lut.id).expect("writing to a string");
        escape_string(output, &lut.name);
        write!(
            output,
            "\", \"kind\": \"{}\", \"size\": {}}}",
            cube_kind(lut.kind),
            lut.size
        )
        .expect("writing to a string");
    }
    output.push_str("],\n    \"grades\": [");
    for (index, entry) in grading.grades.iter().enumerate() {
        if index != 0 {
            output.push_str(", ");
        }
        write!(output, "{{\"id\": {}, \"name\": \"", entry.id).expect("writing to a string");
        escape_string(output, &entry.name);
        output.push_str("\", \"grade\": ");
        write_grade(output, &entry.grade);
        output.push('}');
    }
    output.push_str("],\n    \"input_grades\": [");
    for (index, (input, source)) in grading.inputs.iter().enumerate() {
        if index != 0 {
            output.push_str(", ");
        }
        write!(output, "{{\"input\": {input}, \"grade\": ").expect("writing to a string");
        write_source(output, source);
        output.push('}');
    }
    output.push_str("],\n    \"program_grade\": ");
    match &grading.program {
        Some(source) => write_source(output, source),
        None => output.push_str("null"),
    }
}

const fn cube_kind(kind: CubeKind) -> &'static str {
    match kind {
        CubeKind::OneD => "1d",
        CubeKind::ThreeD => "3d",
    }
}

fn write_source(output: &mut String, source: &GradeSource) {
    match source {
        GradeSource::Library(id) => {
            write!(output, "{{\"type\": \"library\", \"grade\": {id}}}")
                .expect("writing to a string");
        }
        GradeSource::Inline(grade) => {
            output.push_str("{\"type\": \"inline\", \"grade\": ");
            write_grade(output, grade);
            output.push('}');
        }
    }
}

fn write_grade(output: &mut String, grade: &Grade) {
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

/// Parses the four grading fields from the project object. Bounds, names,
/// and references are checked by project validation.
pub(crate) fn parse_grading(object: &mut Object) -> Result<ProjectGrading, DecodeError> {
    let luts = parse_array(object.take("luts")?, "luts", parse_lut)?;
    let grades = parse_array(object.take("grades")?, "grades", parse_library_grade)?;
    let mut inputs = BTreeMap::new();
    for (input, source) in parse_array(
        object.take("input_grades")?,
        "input_grades",
        parse_input_grade,
    )? {
        if inputs.insert(input, source).is_some() {
            return Err(syntax(format!(
                "input_grades must not list input {input} twice"
            )));
        }
    }
    let program = parse_optional(object.take("program_grade")?, parse_source)?;
    Ok(ProjectGrading {
        grades,
        luts,
        inputs,
        program,
    })
}

fn nonzero_u32(object: &mut Object, field: &str) -> Result<NonZeroU32, DecodeError> {
    NonZeroU32::new(object.u32(field)?)
        .ok_or_else(|| syntax(format!("field `{field}` must be nonzero")))
}

fn i16_field(object: &mut Object, field: &str) -> Result<i16, DecodeError> {
    i16::try_from(object.i32(field)?).map_err(|_| syntax(format!("field `{field}` exceeds i16")))
}

fn u16_field(object: &mut Object, field: &str) -> Result<u16, DecodeError> {
    u16::try_from(object.u32(field)?).map_err(|_| syntax(format!("field `{field}` exceeds u16")))
}

fn parse_lut(value: Value) -> Result<ProjectLut, DecodeError> {
    let mut object = Object::new(value, "lut")?;
    let id = LutId::new(nonzero_u32(&mut object, "id")?);
    let name = object.string("name")?;
    let kind = match object.string("kind")?.as_str() {
        "1d" => CubeKind::OneD,
        "3d" => CubeKind::ThreeD,
        value => return Err(unknown_enum("lut kind", value)),
    };
    let size = object.u32("size")?;
    object.finish()?;
    Ok(ProjectLut {
        id,
        name,
        kind,
        size,
    })
}

fn parse_library_grade(value: Value) -> Result<ProjectGrade, DecodeError> {
    let mut object = Object::new(value, "grade library entry")?;
    let id = GradeId::new(nonzero_u32(&mut object, "id")?);
    let name = object.string("name")?;
    let grade = parse_grade(object.take("grade")?)?;
    object.finish()?;
    Ok(ProjectGrade { id, name, grade })
}

fn parse_input_grade(value: Value) -> Result<(InputId, GradeSource), DecodeError> {
    let mut object = Object::new(value, "input grade")?;
    let input = InputId::new(object.nonzero_u128("input")?);
    let source = parse_source(object.take("grade")?)?;
    object.finish()?;
    Ok((input, source))
}

fn parse_source(value: Value) -> Result<GradeSource, DecodeError> {
    let mut object = Object::new(value, "grade source")?;
    let source = match object.string("type")?.as_str() {
        "library" => GradeSource::Library(GradeId::new(nonzero_u32(&mut object, "grade")?)),
        "inline" => GradeSource::Inline(Box::new(parse_grade(object.take("grade")?)?)),
        value => return Err(unknown_enum("grade source type", value)),
    };
    object.finish()?;
    Ok(source)
}

fn parse_grade(value: Value) -> Result<Grade, DecodeError> {
    let mut object = Object::new(value, "grade")?;
    let enabled = object.boolean("enabled")?;
    let mut wb = Object::new(object.take("white_balance")?, "white balance")?;
    let white_balance = WhiteBalance::new(
        i16_field(&mut wb, "temperature")?,
        i16_field(&mut wb, "tint")?,
    );
    wb.finish()?;
    let exposure_millistops = i16_field(&mut object, "exposure_millistops")?;
    let mut contrast = Object::new(object.take("contrast")?, "contrast")?;
    let contrast_value = Contrast::new(
        i16_field(&mut contrast, "amount")?,
        u16_field(&mut contrast, "pivot_bp")?,
    );
    contrast.finish()?;
    let saturation_permille = u16_field(&mut object, "saturation_permille")?;
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
        let strength_permille = u16_field(&mut lut, "strength_permille")?;
        lut.finish()?;
        Ok(GradeLut::new(asset_uri, strength_permille))
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

fn parse_wheel(value: Value) -> Result<Wheel, DecodeError> {
    let mut object = Object::new(value, "wheel")?;
    let wheel = Wheel::new(
        i16_field(&mut object, "x")?,
        i16_field(&mut object, "y")?,
        i16_field(&mut object, "master")?,
    );
    object.finish()?;
    Ok(wheel)
}

fn parse_curve(value: Value) -> Result<Curve, DecodeError> {
    parse_array(value, "curve", |value| {
        let mut object = Object::new(value, "curve point")?;
        let point = CurvePoint::new(u16_field(&mut object, "x")?, u16_field(&mut object, "y")?);
        object.finish()?;
        Ok(point)
    })
    .map(Curve::new)
}

fn parse_hue_curve(value: Value) -> Result<HueCurve, DecodeError> {
    parse_array(value, "hue curve", |value| {
        let mut object = Object::new(value, "hue curve point")?;
        let point = HuePoint::new(
            u16_field(&mut object, "position")?,
            i16_field(&mut object, "value")?,
        );
        object.finish()?;
        Ok(point)
    })
    .map(HueCurve::new)
}
