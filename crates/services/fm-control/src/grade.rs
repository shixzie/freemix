//! Wire mapping of the grade commands, the `grade_changed` event, and the
//! `grade_state` records that follow a snapshot (Protocol 2.28; scene layer
//! grades 2.33).

use std::sync::Arc;

use fm_engine::{Engine, EngineCommand, EngineGradeEdit};
use fm_model::{GradeId, GradeSource, LayerId, LutId, ProjectGrade, SceneLayerRef};
use fm_protocol::{
    CommandPayload, EventPayload, GradeChange, GradeEntry, GradeStateMessage, WireGradeSource,
    WireInputId, WireSceneLayerId,
};

// The wire carries exactly the grading the project holds, so the protocol's
// bounds must be the model's bounds.
const _: () = {
    assert!(fm_protocol::MAX_GRADE_NAME_BYTES == fm_model::MAX_GRADE_NAME_BYTES);
    assert!(fm_protocol::MAX_GRADE_NAME_BYTES == fm_model::MAX_LUT_NAME_BYTES);
    assert!(
        fm_protocol::MAX_GRADE_STATES as usize
            == fm_model::MAX_PROJECT_LUTS
                + fm_model::MAX_PROJECT_GRADES
                + 256
                + fm_model::MAX_PROJECT_LAYER_GRADES
                + 1
    );
    assert!(fm_protocol::MAX_GRADE_LAYERS as usize == fm_model::MAX_PROJECT_LAYER_GRADES);
    assert!(fm_model::MAX_PROJECT_GRADES <= fm_protocol::MAX_LIST_ITEMS);
    assert!(fm_protocol::MAX_GRADE_LUT_IMPORT_BYTES as u64 <= fm_model::MAX_LUT_FILE_BYTES);
};

fn domain_source(source: &WireGradeSource) -> GradeSource {
    match source {
        WireGradeSource::Library(id) => GradeSource::Library(GradeId::new(*id)),
        WireGradeSource::Inline(grade) => GradeSource::Inline(grade.clone()),
    }
}

fn wire_source(source: &GradeSource) -> WireGradeSource {
    match source {
        GradeSource::Library(id) => WireGradeSource::Library(id.get()),
        GradeSource::Inline(grade) => WireGradeSource::Inline(grade.clone()),
    }
}

/// Maps the grade commands.
pub fn grade_engine_command(payload: &CommandPayload) -> EngineCommand {
    EngineCommand::Grade(match payload {
        CommandPayload::SetInputGrade { input, source } => EngineGradeEdit::SetInput {
            input: input.to_domain(),
            source: source.as_ref().map(domain_source),
        },
        CommandPayload::SetLayerGrade {
            input,
            layer,
            source,
        } => EngineGradeEdit::SetLayer {
            input: input.to_domain(),
            layer: LayerId::from_non_zero(layer.get()),
            source: source.as_ref().map(domain_source),
        },
        CommandPayload::SetProgramGrade { source } => EngineGradeEdit::SetProgram {
            source: source.as_ref().map(domain_source),
        },
        CommandPayload::AddLibraryGrade {
            grade,
            name,
            value,
            index,
        } => EngineGradeEdit::AddGrade {
            grade: Box::new(ProjectGrade {
                id: GradeId::new(*grade),
                name: name.clone(),
                grade: (**value).clone(),
            }),
            index: index.map(usize::from),
        },
        CommandPayload::UpdateLibraryGrade { grade, name, value } => EngineGradeEdit::UpdateGrade {
            id: GradeId::new(*grade),
            name: name.clone(),
            grade: value.clone(),
        },
        CommandPayload::RemoveLibraryGrade { grade } => EngineGradeEdit::RemoveGrade {
            id: GradeId::new(*grade),
        },
        CommandPayload::ImportGradeLut { lut, name, data } => EngineGradeEdit::ImportLut {
            id: LutId::new(*lut),
            name: name.clone(),
            source: Arc::from(data.as_str()),
        },
        CommandPayload::RemoveGradeLut { lut } => EngineGradeEdit::RemoveLut {
            id: LutId::new(*lut),
        },
        _ => unreachable!("only grade commands are delegated"),
    })
}

/// Maps an undoable grade edit back to its exact wire payload.
pub fn wire_grade_payload(edit: &EngineGradeEdit) -> CommandPayload {
    match edit {
        EngineGradeEdit::SetInput { input, source } => CommandPayload::SetInputGrade {
            input: WireInputId::from_domain(*input),
            source: source.as_ref().map(wire_source),
        },
        EngineGradeEdit::SetLayer {
            input,
            layer,
            source,
        } => CommandPayload::SetLayerGrade {
            input: WireInputId::from_domain(*input),
            layer: WireSceneLayerId::new(layer.non_zero()),
            source: source.as_ref().map(wire_source),
        },
        EngineGradeEdit::SetProgram { source } => CommandPayload::SetProgramGrade {
            source: source.as_ref().map(wire_source),
        },
        EngineGradeEdit::AddGrade { grade, index } => CommandPayload::AddLibraryGrade {
            grade: grade.id.get(),
            name: grade.name.clone(),
            value: Box::new(grade.grade.clone()),
            index: index.and_then(|index| u16::try_from(index).ok()),
        },
        EngineGradeEdit::UpdateGrade { id, name, grade } => CommandPayload::UpdateLibraryGrade {
            grade: id.get(),
            name: name.clone(),
            value: grade.clone(),
        },
        EngineGradeEdit::RemoveGrade { id } => {
            CommandPayload::RemoveLibraryGrade { grade: id.get() }
        }
        EngineGradeEdit::ImportLut { id, name, source } => CommandPayload::ImportGradeLut {
            lut: id.get(),
            name: name.clone(),
            data: source.to_string(),
        },
        EngineGradeEdit::RemoveLut { id } => CommandPayload::RemoveGradeLut { lut: id.get() },
    }
}

/// The `grade_changed` payload for an accepted `edit` in the post-command
/// `engine`.
pub fn grade_changed(engine: &Engine, edit: &EngineGradeEdit) -> EventPayload {
    let grading = engine.show().grading();
    let library = |id: GradeId| {
        let index = grading
            .grades
            .iter()
            .position(|candidate| candidate.id == id)
            .expect("an accepted library edit leaves the grade in the library");
        let grade = &grading.grades[index];
        GradeChange::Library {
            grade: id.get(),
            index: u16::try_from(index).expect("the library is bounded"),
            name: grade.name.clone(),
            value: Box::new(grade.grade.clone()),
        }
    };
    EventPayload::GradeChanged(match edit {
        EngineGradeEdit::SetInput { input, .. } => GradeChange::Input {
            input: WireInputId::from_domain(*input),
            source: grading.inputs.get(input).map(wire_source),
        },
        EngineGradeEdit::SetLayer { input, layer, .. } => {
            let scene = engine
                .show()
                .scene_inputs()
                .get(input)
                .copied()
                .expect("an accepted layer grade edit names a Scene input");
            GradeChange::Layer {
                inputs: scene_inputs(engine, scene),
                layer: WireSceneLayerId::new(layer.non_zero()),
                source: grading
                    .layers
                    .get(&SceneLayerRef::new(scene, *layer))
                    .map(wire_source),
            }
        }
        EngineGradeEdit::SetProgram { .. } => GradeChange::Program {
            source: grading.program.as_ref().map(wire_source),
        },
        EngineGradeEdit::AddGrade { grade, .. } => library(grade.id),
        EngineGradeEdit::UpdateGrade { id, .. } => library(*id),
        EngineGradeEdit::RemoveGrade { id } => GradeChange::LibraryRemoved { grade: id.get() },
        EngineGradeEdit::ImportLut { id, .. } => {
            let lut = grading
                .lut(*id)
                .expect("an accepted import registers the LUT");
            GradeChange::LutImported {
                lut: id.get(),
                name: lut.name.clone(),
                kind: lut.kind,
                size: lut.size,
            }
        }
        EngineGradeEdit::RemoveLut { id } => GradeChange::LutRemoved { lut: id.get() },
    })
}

/// Every Scene input (ascending) of `engine` showing `scene`.
fn scene_inputs(engine: &Engine, scene: fm_types::SceneId) -> Vec<WireInputId> {
    engine
        .show()
        .scene_inputs()
        .iter()
        .filter(|(_, shown)| **shown == scene)
        .map(|(input, _)| WireInputId::from_domain(*input))
        .collect()
}

/// Every layer grade of a scene some Scene input of `engine` shows, in scene
/// then layer order, with those Scene inputs.
fn replicated_layers(engine: &Engine) -> impl Iterator<Item = GradeEntry> + '_ {
    engine
        .show()
        .grading()
        .layers
        .iter()
        .filter_map(|(target, source)| {
            let inputs = scene_inputs(engine, target.scene);
            (!inputs.is_empty()).then(|| GradeEntry::Layer {
                inputs,
                layer: WireSceneLayerId::new(target.layer.non_zero()),
                source: wire_source(source),
            })
        })
}

/// Every grading item of `engine` as the `grade_state` records that follow
/// a snapshot: LUTs, library grades, input grades, the layer grades of
/// scenes a Scene input shows, then the program grade.
pub fn grade_states(engine: &Engine) -> Vec<GradeStateMessage> {
    let revision = engine.revision().get();
    let grading = engine.show().grading();
    let luts = grading.luts.iter().map(|lut| GradeEntry::Lut {
        lut: lut.id.get(),
        name: lut.name.clone(),
        kind: lut.kind,
        size: lut.size,
    });
    let library = grading.grades.iter().map(|grade| GradeEntry::Library {
        grade: grade.id.get(),
        name: grade.name.clone(),
        value: Box::new(grade.grade.clone()),
    });
    let inputs = grading
        .inputs
        .iter()
        .map(|(input, source)| GradeEntry::Input {
            input: WireInputId::from_domain(*input),
            source: wire_source(source),
        });
    let program = grading.program.iter().map(|source| GradeEntry::Program {
        source: wire_source(source),
    });
    luts.chain(library)
        .chain(inputs)
        .chain(replicated_layers(engine))
        .chain(program)
        .map(|entry| GradeStateMessage { revision, entry })
        .collect()
}

/// How many `grade_state` records follow a snapshot of `engine`.
pub fn grade_state_count(engine: &Engine) -> u16 {
    let grading = engine.show().grading();
    let count = grading.luts.len()
        + grading.grades.len()
        + grading.inputs.len()
        + replicated_layers(engine).count()
        + usize::from(grading.program.is_some());
    u16::try_from(count).expect("project grading is bounded")
}
