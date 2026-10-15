//! Project grades compiled for rendering.
//!
//! [`CompiledProjectGrades`] turns a project's per-input and program grades
//! into shared [`CompiledGrade`]s that scene builders attach to compositor
//! layers (`SourceLayer::with_grade`) and to the composed Program picture.
//! A library grade used by several targets is compiled once and shared.
//! Identity grades are omitted, so an ungraded or neutral target renders
//! bit-exactly as before. A referenced LUT missing from the supplied set is
//! bypassed by its grade and reported through
//! [`CompiledProjectGrades::missing_luts`].

use core::fmt;
use std::{collections::BTreeMap, sync::Arc};

use fm_grade::{CompiledGrade, CubeLut, Grade, GradeError};
use fm_model::{GradeId, GradeSource, LutId, Project, ProjectGrading, lut_id_from_uri};
use fm_types::InputId;

/// Compiled per-input and program grades of one project.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CompiledProjectGrades {
    inputs: BTreeMap<InputId, Arc<CompiledGrade>>,
    program: Option<Arc<CompiledGrade>>,
    missing_luts: Vec<LutId>,
}

/// A project grade that cannot be compiled.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GradeCompileError {
    /// A target references a library grade the project does not hold.
    UnknownGrade(GradeId),
    /// A grade violates an `fm_grade` bound.
    Invalid {
        target: GradeTarget,
        error: GradeError,
    },
}

/// What a project grade colours.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GradeTarget {
    Input(InputId),
    Program,
}

impl fmt::Display for GradeCompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownGrade(id) => write!(formatter, "grade {id} does not exist"),
            Self::Invalid {
                target: GradeTarget::Input(input),
                error,
            } => write!(formatter, "input {input} grade is invalid: {error}"),
            Self::Invalid {
                target: GradeTarget::Program,
                error,
            } => write!(formatter, "program grade is invalid: {error}"),
        }
    }
}

impl std::error::Error for GradeCompileError {}

impl CompiledProjectGrades {
    /// Compiles every input and program grade of `project` against the
    /// parsed LUTs in `luts`.
    ///
    /// # Errors
    ///
    /// Returns [`GradeCompileError`] for a dangling library reference or a
    /// grade outside its bounds.
    pub fn compile(
        project: &Project,
        luts: &BTreeMap<LutId, Arc<CubeLut>>,
    ) -> Result<Self, GradeCompileError> {
        Self::compile_grading(project.grading(), luts)
    }

    /// Compiles every input and program grade of `grading` against the
    /// parsed LUTs in `luts`.
    ///
    /// # Errors
    ///
    /// Returns [`GradeCompileError`] for a dangling library reference or a
    /// grade outside its bounds.
    pub fn compile_grading(
        grading: &ProjectGrading,
        luts: &BTreeMap<LutId, Arc<CubeLut>>,
    ) -> Result<Self, GradeCompileError> {
        let mut compiler = Compiler {
            luts,
            library: BTreeMap::new(),
            missing_luts: Vec::new(),
        };
        let mut inputs = BTreeMap::new();
        for (input, source) in &grading.inputs {
            let grade = match source {
                GradeSource::Library(id) => {
                    let entry = grading
                        .grade(*id)
                        .ok_or(GradeCompileError::UnknownGrade(*id))?;
                    compiler.library(*id, &entry.grade, GradeTarget::Input(*input))?
                }
                GradeSource::Inline(grade) => {
                    compiler.compile(grade, GradeTarget::Input(*input))?
                }
            };
            if !grade.is_identity() {
                inputs.insert(*input, grade);
            }
        }
        let program = match &grading.program {
            None => None,
            Some(GradeSource::Library(id)) => {
                let entry = grading
                    .grade(*id)
                    .ok_or(GradeCompileError::UnknownGrade(*id))?;
                Some(compiler.library(*id, &entry.grade, GradeTarget::Program)?)
            }
            Some(GradeSource::Inline(grade)) => {
                Some(compiler.compile(grade, GradeTarget::Program)?)
            }
        }
        .filter(|grade| !grade.is_identity());
        let mut missing_luts = compiler.missing_luts;
        missing_luts.sort_unstable();
        missing_luts.dedup();
        Ok(Self {
            inputs,
            program,
            missing_luts,
        })
    }

    /// The non-identity grade of `input`, if any.
    #[must_use]
    pub fn input(&self, input: InputId) -> Option<&Arc<CompiledGrade>> {
        self.inputs.get(&input)
    }

    /// Every graded input with its non-identity grade, in id order.
    #[must_use]
    pub const fn inputs(&self) -> &BTreeMap<InputId, Arc<CompiledGrade>> {
        &self.inputs
    }

    /// The non-identity program grade, if any.
    #[must_use]
    pub const fn program(&self) -> Option<&Arc<CompiledGrade>> {
        self.program.as_ref()
    }

    /// Referenced LUTs absent from the supplied set, whose stage is bypassed.
    #[must_use]
    pub fn missing_luts(&self) -> &[LutId] {
        &self.missing_luts
    }

    /// Whether no input and not the program is graded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inputs.is_empty() && self.program.is_none()
    }
}

struct Compiler<'a> {
    luts: &'a BTreeMap<LutId, Arc<CubeLut>>,
    library: BTreeMap<GradeId, Arc<CompiledGrade>>,
    missing_luts: Vec<LutId>,
}

impl Compiler<'_> {
    fn library(
        &mut self,
        id: GradeId,
        grade: &Grade,
        target: GradeTarget,
    ) -> Result<Arc<CompiledGrade>, GradeCompileError> {
        if let Some(compiled) = self.library.get(&id) {
            return Ok(Arc::clone(compiled));
        }
        let compiled = self.compile(grade, target)?;
        self.library.insert(id, Arc::clone(&compiled));
        Ok(compiled)
    }

    fn compile(
        &mut self,
        grade: &Grade,
        target: GradeTarget,
    ) -> Result<Arc<CompiledGrade>, GradeCompileError> {
        let id = grade
            .lut
            .as_ref()
            .and_then(|reference| lut_id_from_uri(&reference.asset_uri));
        let lut = id.and_then(|id| self.luts.get(&id).cloned());
        let compiled = CompiledGrade::new(grade, lut)
            .map_err(|error| GradeCompileError::Invalid { target, error })?;
        if compiled.lut_missing()
            && let Some(id) = id
        {
            self.missing_luts.push(id);
        }
        Ok(Arc::new(compiled))
    }
}
