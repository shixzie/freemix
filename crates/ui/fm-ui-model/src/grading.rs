//! The replicated project grading (Protocol 2.28): imported LUTs, the grade
//! library, and the per-input, per-scene-layer (Protocol 2.33), and program
//! grades, as carried by a snapshot's `grade_state` records and kept current
//! by `grade_changed`.

use core::num::NonZeroU32;
use std::collections::BTreeMap;

use fm_grade::{CubeKind, Grade};
use fm_protocol::{GradeChange, GradeEntry, WireGradeSource, WireSceneLayerId};
use fm_types::InputId;

/// One imported LUT.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GradeLutStatus {
    pub lut: NonZeroU32,
    pub name: String,
    pub kind: CubeKind,
    /// Edge length for a 3D LUT, entry count for a 1D LUT.
    pub size: u32,
}

/// One named library grade.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LibraryGradeStatus {
    pub grade: NonZeroU32,
    pub name: String,
    pub value: Box<Grade>,
}

/// The replicated project grading.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GradingStatus {
    /// Imported LUTs in import order.
    pub luts: Vec<GradeLutStatus>,
    /// Library grades in library order.
    pub library: Vec<LibraryGradeStatus>,
    /// The grade of every graded input.
    pub inputs: BTreeMap<InputId, WireGradeSource>,
    /// The grade of every graded scene layer, under each Scene input
    /// showing its scene (a scene shown by two Scene inputs is listed under
    /// both, with the same grade).
    pub layers: BTreeMap<(InputId, WireSceneLayerId), WireGradeSource>,
    /// The program output grade, if any.
    pub program: Option<WireGradeSource>,
}

impl GradingStatus {
    /// The library grade `grade`, if any.
    #[must_use]
    pub fn library_grade(&self, grade: NonZeroU32) -> Option<&LibraryGradeStatus> {
        self.library.iter().find(|entry| entry.grade == grade)
    }

    /// The grade `source` evaluates to, or `None` for an unknown library id.
    #[must_use]
    pub fn resolve<'a>(&'a self, source: &'a WireGradeSource) -> Option<&'a Grade> {
        match source {
            WireGradeSource::Library(grade) => {
                self.library_grade(*grade).map(|entry| &*entry.value)
            }
            WireGradeSource::Inline(grade) => Some(grade),
        }
    }

    /// The grade layer `layer` of Scene input `input`'s scene carries, if
    /// any.
    #[must_use]
    pub fn layer(&self, input: InputId, layer: WireSceneLayerId) -> Option<&WireGradeSource> {
        self.layers.get(&(input, layer))
    }

    /// Forgets every grade replicated under a removed input.
    pub(crate) fn remove_input(&mut self, input: InputId) {
        self.inputs.remove(&input);
        self.layers.retain(|(shown, _), _| *shown != input);
    }

    fn source_known(&self, source: &WireGradeSource) -> bool {
        match source {
            WireGradeSource::Library(grade) => self.library_grade(*grade).is_some(),
            WireGradeSource::Inline(_) => true,
        }
    }

    /// Appends one `grade_state` entry, which must follow the documented
    /// order (LUTs, library grades, ascending inputs, scene layers, program)
    /// without repeating an id and name only show inputs and known library
    /// grades.
    pub(crate) fn push_entry(&mut self, entry: GradeEntry, inputs: &[InputId]) -> bool {
        match entry {
            GradeEntry::Lut {
                lut,
                name,
                kind,
                size,
            } => {
                if !self.library.is_empty()
                    || !self.inputs.is_empty()
                    || !self.layers.is_empty()
                    || self.program.is_some()
                    || self.luts.iter().any(|entry| entry.lut == lut)
                {
                    return false;
                }
                self.luts.push(GradeLutStatus {
                    lut,
                    name,
                    kind,
                    size,
                });
            }
            GradeEntry::Library { grade, name, value } => {
                if !self.inputs.is_empty()
                    || !self.layers.is_empty()
                    || self.program.is_some()
                    || self.library_grade(grade).is_some()
                {
                    return false;
                }
                self.library.push(LibraryGradeStatus { grade, name, value });
            }
            GradeEntry::Input { input, source } => {
                let input = input.to_domain();
                if self.program.is_some()
                    || !self.layers.is_empty()
                    || !inputs.contains(&input)
                    || self
                        .inputs
                        .keys()
                        .next_back()
                        .is_some_and(|last| *last >= input)
                    || !self.source_known(&source)
                {
                    return false;
                }
                self.inputs.insert(input, source);
            }
            GradeEntry::Layer {
                inputs: shown,
                layer,
                source,
            } => {
                let shown = shown
                    .iter()
                    .map(|input| input.to_domain())
                    .collect::<Vec<_>>();
                if self.program.is_some()
                    || shown.is_empty()
                    || !shown.iter().all(|input| inputs.contains(input))
                    || shown
                        .iter()
                        .any(|input| self.layers.contains_key(&(*input, layer)))
                    || !self.source_known(&source)
                {
                    return false;
                }
                for input in shown {
                    self.layers.insert((input, layer), source.clone());
                }
            }
            GradeEntry::Program { source } => {
                if self.program.is_some() || !self.source_known(&source) {
                    return false;
                }
                self.program = Some(source);
            }
        }
        true
    }

    /// Whether `change` applies to this grading and the show's `inputs`.
    pub(crate) fn accepts(&self, change: &GradeChange, inputs: &[InputId]) -> bool {
        match change {
            GradeChange::LutImported { lut, .. } => self.luts.iter().all(|entry| entry.lut != *lut),
            GradeChange::LutRemoved { lut } => self.luts.iter().any(|entry| entry.lut == *lut),
            GradeChange::Library { grade, index, .. } => {
                let index = usize::from(*index);
                match self.library.iter().position(|entry| entry.grade == *grade) {
                    Some(current) => current == index,
                    None => index <= self.library.len(),
                }
            }
            GradeChange::LibraryRemoved { grade } => self.library_grade(*grade).is_some(),
            GradeChange::Input { input, source } => {
                inputs.contains(&input.to_domain())
                    && source
                        .as_ref()
                        .is_none_or(|source| self.source_known(source))
            }
            GradeChange::Layer {
                inputs: shown,
                source,
                ..
            } => {
                !shown.is_empty()
                    && shown
                        .iter()
                        .all(|input| inputs.contains(&input.to_domain()))
                    && source
                        .as_ref()
                        .is_none_or(|source| self.source_known(source))
            }
            GradeChange::Program { source } => source
                .as_ref()
                .is_none_or(|source| self.source_known(source)),
        }
    }

    /// Applies an accepted change.
    pub(crate) fn apply(&mut self, change: GradeChange) {
        match change {
            GradeChange::LutImported {
                lut,
                name,
                kind,
                size,
            } => self.luts.push(GradeLutStatus {
                lut,
                name,
                kind,
                size,
            }),
            GradeChange::LutRemoved { lut } => self.luts.retain(|entry| entry.lut != lut),
            GradeChange::Library {
                grade,
                index,
                name,
                value,
            } => {
                let entry = LibraryGradeStatus { grade, name, value };
                match self
                    .library
                    .iter_mut()
                    .find(|candidate| candidate.grade == grade)
                {
                    Some(current) => *current = entry,
                    None => self.library.insert(usize::from(index), entry),
                }
            }
            GradeChange::LibraryRemoved { grade } => {
                self.library.retain(|entry| entry.grade != grade);
            }
            GradeChange::Input { input, source } => match source {
                Some(source) => {
                    self.inputs.insert(input.to_domain(), source);
                }
                None => {
                    self.inputs.remove(&input.to_domain());
                }
            },
            GradeChange::Layer {
                inputs: shown,
                layer,
                source,
            } => {
                for input in shown {
                    let key = (input.to_domain(), layer);
                    match &source {
                        Some(source) => {
                            self.layers.insert(key, source.clone());
                        }
                        None => {
                            self.layers.remove(&key);
                        }
                    }
                }
            }
            GradeChange::Program { source } => self.program = source,
        }
    }
}
