//! Strict `project.json` encoding of the grade library, imported LUTs, and
//! the per-input and program grades.
//!
//! ```text
//! "luts": [{"id": 1, "name": "Film", "kind": "3d", "size": 33}],
//! "grades": [{"id": 1, "name": "Warm", "grade": GRADE}],
//! "input_grades": [{"input": 7, "grade": SOURCE}],
//! "layer_grades": [{"scene": 3, "layer": 2, "grade": SOURCE}],
//! "program_grade": null | SOURCE
//! ```
//!
//! `SOURCE` is `{"type": "library", "grade": 1}` or
//! `{"type": "inline", "grade": GRADE}`. `GRADE` spells out every stage of an
//! [`fm_grade::Grade`] with its exact integers; curve points are
//! `{"x", "y"}` and hue-curve points `{"position", "value"}` objects.

use std::{collections::BTreeMap, fmt::Write as _, num::NonZeroU32};

use fm_grade::{CubeKind, Grade};
use fm_model::{
    GradeId, GradeSource, LayerId, LutId, ProjectGrade, ProjectGrading, ProjectLut, SceneLayerRef,
};
use fm_types::{InputId, SceneId};

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
    output.push_str("],\n    \"layer_grades\": [");
    for (index, (target, source)) in grading.layers.iter().enumerate() {
        if index != 0 {
            output.push_str(", ");
        }
        write!(
            output,
            "{{\"scene\": {}, \"layer\": {}, \"grade\": ",
            target.scene, target.layer
        )
        .expect("writing to a string");
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

pub(crate) fn write_grade(output: &mut String, grade: &Grade) {
    fm_preset_file::grade::write_grade(output, grade);
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
    let mut layers = BTreeMap::new();
    for (target, source) in parse_array(
        object.take("layer_grades")?,
        "layer_grades",
        parse_layer_grade,
    )? {
        if layers.insert(target, source).is_some() {
            return Err(syntax(format!("layer_grades must not list {target} twice")));
        }
    }
    let program = parse_optional(object.take("program_grade")?, parse_source)?;
    Ok(ProjectGrading {
        grades,
        luts,
        inputs,
        layers,
        program,
    })
}

fn nonzero_u32(object: &mut Object, field: &str) -> Result<NonZeroU32, DecodeError> {
    NonZeroU32::new(object.u32(field)?)
        .ok_or_else(|| syntax(format!("field `{field}` must be nonzero")))
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

fn parse_layer_grade(value: Value) -> Result<(SceneLayerRef, GradeSource), DecodeError> {
    let mut object = Object::new(value, "layer grade")?;
    let scene = SceneId::new(object.nonzero_u128("scene")?);
    let layer = LayerId::from_non_zero(nonzero_u32(&mut object, "layer")?);
    let source = parse_source(object.take("grade")?)?;
    object.finish()?;
    Ok((SceneLayerRef::new(scene, layer), source))
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

pub(crate) fn parse_grade(value: Value) -> Result<Grade, DecodeError> {
    Ok(fm_preset_file::grade::parse_grade(value)?)
}
