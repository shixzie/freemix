//! Grade edits: project grading changed through the engine's command path.
//!
//! The show holds the project's [`ProjectGrading`] (grade library, imported
//! LUTs, per-input, per-scene-layer, and program grades) and, for each
//! imported LUT whose file was available, its exact `.cube` text and parsed
//! table ([`EngineLut`]). Every [`EngineGradeEdit`] is a project edit: the
//! edit padlock covers it (an input's padlock also covers that input's
//! grade, and every Scene input's padlock the grades of its scene's layers) and
//! it is recorded in the undo history with an exact inverse, except LUT
//! removal.
//!
//! # Scene layer grades
//!
//! A layer grade belongs to a scene layer ([`SceneLayerRef`]); an edit names
//! the scene directly ([`EngineGradeEdit::SetLayer`]), so a layer of a scene
//! no Scene input shows is edited like any other. Every Scene input showing
//! the scene shows the change and every one of their padlocks covers it.
//! The show knows every project scene ([`crate::ShowState::set_scene`]), so
//! the edit is checked against the scene's layers.
//!
//! # LUT removal is not undoable
//!
//! A LUT loaded from a project bundle may be up to
//! [`fm_model::MAX_LUT_FILE_BYTES`], far more than a protocol command can
//! carry, so re-importing it cannot be journaled. Removing a LUT therefore
//! works like removing an input: it clears the redo stack and forgets every
//! retained undo entry that names the LUT. Undoing an *import* (which carried
//! its bytes in the command) removes the LUT again and is undoable as usual.
//! Because the forgotten entries may include the grade edits of an input
//! whose add is still undoable, undoing an input add is refused as history
//! drift while that input carries any grade (removal would drop it for good).

use core::fmt;
use std::{collections::BTreeMap, sync::Arc};

use fm_command::{FieldIssue, Rejection, RejectionCode};
use fm_grade::{CubeError, CubeLut, Grade, parse_cube};
use fm_model::{
    GradeEditError, GradeId, GradeSource, LayerId, LutId, MAX_LUT_FILE_BYTES, ProjectGrade,
    ProjectGrading, ProjectLut, SceneLayerRef, lut_id_from_uri,
};
use fm_types::{InputId, SceneId};

use crate::ShowState;

/// One imported LUT the show can render: its exact `.cube` text and the
/// table parsed from it. Two LUTs are equal when their texts are.
#[derive(Clone)]
pub struct EngineLut {
    source: Arc<str>,
    cube: Arc<CubeLut>,
}

impl EngineLut {
    /// Parses `.cube` text.
    ///
    /// # Errors
    ///
    /// Returns the parser's error for invalid text.
    pub fn parse(source: Arc<str>) -> Result<Self, CubeError> {
        let (cube, _) = parse_cube(&source)?;
        Ok(Self {
            source,
            cube: Arc::new(cube),
        })
    }

    /// Pairs already-parsed text with its table. The caller guarantees that
    /// `cube` was parsed from `source`.
    #[must_use]
    pub const fn from_parsed(source: Arc<str>, cube: Arc<CubeLut>) -> Self {
        Self { source, cube }
    }

    /// The exact `.cube` text.
    #[must_use]
    pub const fn source(&self) -> &Arc<str> {
        &self.source
    }

    /// The parsed table.
    #[must_use]
    pub const fn cube(&self) -> &Arc<CubeLut> {
        &self.cube
    }
}

impl PartialEq for EngineLut {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
    }
}

impl Eq for EngineLut {}

impl fmt::Debug for EngineLut {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EngineLut")
            .field("bytes", &self.source.len())
            .field("kind", &self.cube.kind())
            .field("size", &self.cube.size())
            .finish()
    }
}

/// The live drag a grade set belongs to.
///
/// A drag sends its grade every few frames so the monitors follow it; each
/// send is one accepted edit. An edit that [`Self::continues`] folds into
/// the latest undo entry when that entry sets the same target and was
/// opened by the same [`Self::token`], keeping the entry's inverse (the
/// grade before the drag), so the whole drag undoes at once. A drag that
/// ends where it started leaves no entry. Recorded entries carry no
/// gesture.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct GradeGesture {
    /// Picked by the sender once per drag, unique across sessions.
    pub token: core::num::NonZeroU64,
    /// `false` for the first edit of the drag.
    pub continues: bool,
}

/// One grading edit. Every variant is a project edit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EngineGradeEdit {
    /// Sets (`Some`) or clears (`None`) one input's grade.
    SetInput {
        input: InputId,
        source: Option<GradeSource>,
        gesture: Option<GradeGesture>,
    },
    /// Sets (`Some`) or clears (`None`) the grade of layer `layer` of scene
    /// `scene`, whether or not a Scene input shows it.
    SetLayer {
        scene: SceneId,
        layer: LayerId,
        source: Option<GradeSource>,
        gesture: Option<GradeGesture>,
    },
    /// Sets (`Some`) or clears (`None`) the program output grade.
    SetProgram {
        source: Option<GradeSource>,
        gesture: Option<GradeGesture>,
    },
    /// Inserts a library grade at `index` (the end when `None`).
    AddGrade {
        grade: Box<ProjectGrade>,
        index: Option<usize>,
    },
    /// Renames a library grade and replaces its values.
    UpdateGrade {
        id: GradeId,
        name: String,
        grade: Box<Grade>,
    },
    /// Removes a library grade nothing references.
    RemoveGrade { id: GradeId },
    /// Imports `.cube` text as LUT `id`; kind and size come from parsing it.
    /// The text is at most [`MAX_LUT_FILE_BYTES`].
    ImportLut {
        id: LutId,
        name: String,
        source: Arc<str>,
    },
    /// Unregisters a LUT nothing references. Not undoable (see the module
    /// documentation).
    RemoveLut { id: LutId },
}

impl EngineGradeEdit {
    /// Whether this edit, or a grade it carries, refers to `lut`.
    #[must_use]
    pub fn names_lut(&self, lut: LutId) -> bool {
        let grade_names = |grade: &Grade| {
            grade
                .lut
                .as_ref()
                .is_some_and(|reference| lut_id_from_uri(&reference.asset_uri) == Some(lut))
        };
        let source_names = |source: &Option<GradeSource>| matches!(source, Some(GradeSource::Inline(grade)) if grade_names(grade));
        match self {
            Self::SetInput { source, .. }
            | Self::SetLayer { source, .. }
            | Self::SetProgram { source, .. } => source_names(source),
            Self::AddGrade { grade, .. } => grade_names(&grade.grade),
            Self::UpdateGrade { grade, .. } => grade_names(grade),
            Self::RemoveGrade { .. } => false,
            Self::ImportLut { id, .. } | Self::RemoveLut { id } => *id == lut,
        }
    }

    /// The live drag of a grade set, if any.
    #[must_use]
    pub const fn gesture(&self) -> Option<GradeGesture> {
        match self {
            Self::SetInput { gesture, .. }
            | Self::SetLayer { gesture, .. }
            | Self::SetProgram { gesture, .. } => *gesture,
            _ => None,
        }
    }

    /// The edit as recorded in the history: in no gesture.
    #[must_use]
    pub fn recorded(mut self) -> Self {
        if let Self::SetInput { gesture, .. }
        | Self::SetLayer { gesture, .. }
        | Self::SetProgram { gesture, .. } = &mut self
        {
            *gesture = None;
        }
        self
    }

    /// Whether both edits are grade sets of the same target (one input,
    /// one scene layer, or the program).
    #[must_use]
    pub fn sets_same_target(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::SetInput { input, .. }, Self::SetInput { input: other, .. }) => input == other,
            (
                Self::SetLayer { scene, layer, .. },
                Self::SetLayer {
                    scene: other_scene,
                    layer: other_layer,
                    ..
                },
            ) => scene == other_scene && layer == other_layer,
            (Self::SetProgram { .. }, Self::SetProgram { .. }) => true,
            _ => false,
        }
    }

    /// The grade a grade set leaves on its target (`None` clears it); `None`
    /// for other edits.
    #[must_use]
    pub const fn set_source(&self) -> Option<&Option<GradeSource>> {
        match self {
            Self::SetInput { source, .. }
            | Self::SetLayer { source, .. }
            | Self::SetProgram { source, .. } => Some(source),
            _ => None,
        }
    }

    /// The input this edit grades, if any.
    #[must_use]
    pub const fn input(&self) -> Option<InputId> {
        match self {
            Self::SetInput { input, .. } => Some(*input),
            _ => None,
        }
    }
}

/// Layer `layer` of scene `scene`, checked against the show's scenes.
pub(crate) fn layer_target(
    state: &ShowState,
    scene: SceneId,
    layer: LayerId,
) -> Result<SceneLayerRef, Rejection> {
    let shown = state
        .scene(scene)
        .ok_or_else(|| grade_rejection(&GradeEditError::UnknownScene(scene)))?;
    let target = SceneLayerRef::new(scene, layer);
    if shown.scene.layer_by_id(layer).is_none() {
        return Err(grade_rejection(&GradeEditError::UnknownLayer(target)));
    }
    Ok(target)
}

/// Applies one grade edit to the show.
pub(crate) fn apply_show_grade_edit(
    state: &mut ShowState,
    edit: &EngineGradeEdit,
) -> Result<(), Rejection> {
    if let EngineGradeEdit::SetLayer {
        scene,
        layer,
        source,
        ..
    } = edit
    {
        let target = layer_target(state, *scene, *layer)?;
        let (_, grading, _) = state.grading_parts_mut();
        return grading
            .set_layer(target, source.clone())
            .map_err(|error| grade_rejection(&error));
    }
    let (inputs, grading, luts) = state.grading_parts_mut();
    apply_grade_edit(inputs, grading, luts, edit)
}

/// Validates that `grading` and `luts` form a consistent show grading for
/// `inputs`: every LUT, grade, and reference passes the same checks as an
/// edit, every graded input exists, and every parsed LUT is registered.
/// Layer grades are checked as grades; their scenes and layers are the
/// project's to check (scenes are registered with the show separately).
pub(crate) fn checked_grading(
    inputs: &[InputId],
    grading: ProjectGrading,
    luts: &BTreeMap<LutId, EngineLut>,
) -> Result<ProjectGrading, GradeEditError> {
    let mut checked = ProjectGrading::default();
    for lut in grading.luts {
        checked.add_lut(lut)?;
    }
    for grade in grading.grades {
        checked.insert_grade(None, grade)?;
    }
    for (input, source) in grading.inputs {
        if !inputs.contains(&input) {
            return Err(GradeEditError::UnknownInput(input));
        }
        checked.set_input(input, Some(source))?;
    }
    for (target, source) in grading.layers {
        checked.set_layer(target, Some(source))?;
    }
    checked.set_program(grading.program)?;
    if let Some(unknown) = luts.keys().find(|id| checked.lut(**id).is_none()) {
        return Err(GradeEditError::UnknownLut(*unknown));
    }
    for (id, lut) in luts {
        let registered = checked.lut(*id).expect("checked above");
        if lut.cube.kind() != registered.kind
            || u32::try_from(lut.cube.size()).ok() != Some(registered.size)
        {
            return Err(GradeEditError::LutSizeOutOfRange {
                kind: lut.cube.kind(),
                size: u32::try_from(lut.cube.size()).unwrap_or(u32::MAX),
            });
        }
    }
    Ok(checked)
}

/// Applies one grade edit to the show's grading and LUTs.
pub(crate) fn apply_grade_edit(
    inputs: &[InputId],
    grading: &mut ProjectGrading,
    luts: &mut BTreeMap<LutId, EngineLut>,
    edit: &EngineGradeEdit,
) -> Result<(), Rejection> {
    match edit {
        EngineGradeEdit::SetInput { input, source, .. } => {
            if !inputs.contains(input) {
                return Err(grade_rejection(&GradeEditError::UnknownInput(*input)));
            }
            grading
                .set_input(*input, source.clone())
                .map_err(|error| grade_rejection(&error))
        }
        EngineGradeEdit::SetLayer { .. } => {
            unreachable!("layer grade edits are applied by apply_show_grade_edit")
        }
        EngineGradeEdit::SetProgram { source, .. } => grading
            .set_program(source.clone())
            .map_err(|error| grade_rejection(&error)),
        EngineGradeEdit::AddGrade { grade, index } => grading
            .insert_grade(*index, (**grade).clone())
            .map_err(|error| grade_rejection(&error)),
        EngineGradeEdit::UpdateGrade { id, name, grade } => grading
            .update_grade(*id, name.clone(), (**grade).clone())
            .map_err(|error| grade_rejection(&error)),
        EngineGradeEdit::RemoveGrade { id } => grading
            .remove_grade(*id)
            .map(|_| ())
            .map_err(|error| grade_rejection(&error)),
        EngineGradeEdit::ImportLut { id, name, source } => {
            if u64::try_from(source.len()).unwrap_or(u64::MAX) > MAX_LUT_FILE_BYTES {
                return Err(Rejection::new(
                    RejectionCode::InvalidCommand,
                    format!("LUT text exceeds the {MAX_LUT_FILE_BYTES}-byte limit"),
                )
                .with_field(FieldIssue::new(
                    "data",
                    "too_large",
                    "the LUT is too large",
                )));
            }
            let lut = EngineLut::parse(Arc::clone(source)).map_err(|error| {
                let message = format!("invalid .cube LUT: {error}");
                Rejection::new(RejectionCode::InvalidCommand, message.clone())
                    .with_field(FieldIssue::new("data", "invalid_cube", message))
            })?;
            let size = u32::try_from(lut.cube.size()).map_err(|_| {
                grade_rejection(&GradeEditError::LutSizeOutOfRange {
                    kind: lut.cube.kind(),
                    size: u32::MAX,
                })
            })?;
            grading
                .add_lut(ProjectLut {
                    id: *id,
                    name: name.clone(),
                    kind: lut.cube.kind(),
                    size,
                })
                .map_err(|error| grade_rejection(&error))?;
            luts.insert(*id, lut);
            Ok(())
        }
        EngineGradeEdit::RemoveLut { id } => {
            grading
                .remove_lut(*id)
                .map_err(|error| grade_rejection(&error))?;
            luts.remove(id);
            Ok(())
        }
    }
}

/// Converts a refused grading edit into a receipt rejection with a stable
/// field code.
pub(crate) fn grade_rejection(error: &GradeEditError) -> Rejection {
    let (code, field, issue) = match error {
        GradeEditError::UnknownInput(_) => (RejectionCode::NotFound, "input", "unknown_input"),
        GradeEditError::UnknownScene(_) => (RejectionCode::NotFound, "scene", "unknown_scene"),
        GradeEditError::UnknownLayer(_) => (RejectionCode::NotFound, "layer", "unknown_layer"),
        GradeEditError::UnknownGrade(_) => (RejectionCode::NotFound, "grade", "unknown_grade"),
        GradeEditError::UnknownLut(_) => (RejectionCode::NotFound, "lut", "unknown_lut"),
        GradeEditError::DuplicateGradeId(_) => {
            (RejectionCode::Conflict, "grade", "duplicate_grade")
        }
        GradeEditError::DuplicateLutId(_) => (RejectionCode::Conflict, "lut", "duplicate_lut"),
        GradeEditError::DuplicateName(_) => (RejectionCode::Conflict, "name", "duplicate_name"),
        GradeEditError::EmptyName
        | GradeEditError::NameTooLong { .. }
        | GradeEditError::NameControlCharacter => {
            (RejectionCode::InvalidCommand, "name", "invalid_name")
        }
        GradeEditError::TooManyGrades { .. } => {
            (RejectionCode::Conflict, "grade", "too_many_grades")
        }
        GradeEditError::TooManyLuts { .. } => (RejectionCode::Conflict, "lut", "too_many_luts"),
        GradeEditError::TooManyLayerGrades { .. } => {
            (RejectionCode::Conflict, "layer", "too_many_layer_grades")
        }
        GradeEditError::LutSizeOutOfRange { .. } => {
            (RejectionCode::InvalidCommand, "data", "invalid_cube")
        }
        GradeEditError::InvalidGrade(_) => (RejectionCode::InvalidCommand, "grade", "invalid"),
        GradeEditError::UnregisteredLut(_) => {
            (RejectionCode::NotFound, "grade", "unregistered_lut")
        }
        GradeEditError::GradeInUse(_) => (RejectionCode::Conflict, "grade", "grade_in_use"),
        GradeEditError::LutInUse(_) => (RejectionCode::Conflict, "lut", "lut_in_use"),
        GradeEditError::IndexOutOfRange { .. } => {
            (RejectionCode::InvalidCommand, "index", "out_of_range")
        }
    };
    let message = error.to_string();
    let rejection =
        Rejection::new(code, message.clone()).with_field(FieldIssue::new(field, issue, message));
    match grade_reason(error) {
        Some(reason) => rejection.with_reason(reason),
        None => rejection,
    }
}

/// The typed refusal reason of a grading edit, where it has one.
fn grade_reason(error: &GradeEditError) -> Option<fm_command::RefusalReason> {
    use fm_command::{IdKind, RefusalReason};
    let id = |kind, id: u128| RefusalReason::UnknownId { kind, id };
    let duplicate = |kind, id: u128| RefusalReason::DuplicateId { kind, id };
    Some(match error {
        GradeEditError::UnknownInput(input) => crate::error::unknown_input_reason(*input),
        GradeEditError::UnknownScene(scene) => id(IdKind::Scene, scene.get().get()),
        GradeEditError::UnknownGrade(grade) => id(IdKind::Grade, u128::from(grade.get().get())),
        GradeEditError::UnknownLut(lut) => id(IdKind::Lut, u128::from(lut.get().get())),
        GradeEditError::DuplicateGradeId(grade) => {
            duplicate(IdKind::Grade, u128::from(grade.get().get()))
        }
        GradeEditError::DuplicateLutId(lut) => duplicate(IdKind::Lut, u128::from(lut.get().get())),
        GradeEditError::DuplicateName(_) => RefusalReason::DuplicateName,
        GradeEditError::NameTooLong { maximum, .. } => RefusalReason::InvalidName {
            max_bytes: u32::try_from(*maximum).unwrap_or(u32::MAX),
        },
        GradeEditError::TooManyGrades { maximum } => {
            RefusalReason::bounds("grades", *maximum as u64)
        }
        GradeEditError::TooManyLuts { maximum } => RefusalReason::bounds("luts", *maximum as u64),
        GradeEditError::TooManyLayerGrades { maximum } => {
            RefusalReason::bounds("layer_grades", *maximum as u64)
        }
        _ => return None,
    })
}
