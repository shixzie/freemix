//! Project colour grading: a named grade library, imported `.cube` LUTs, and
//! the grade each input, each scene layer, and the program output carry.
//!
//! Grades are [`fm_grade::Grade`] values. A grade is either kept in the
//! project library ([`ProjectGrade`], referenced by [`GradeId`]) or stored
//! inline on its target. A grade's LUT stage names an imported LUT through
//! its exact asset URI, `asset://luts/<id>.cube` ([`lut_asset_uri`]); the LUT
//! file lives at `assets/luts/<id>.cube` in the project bundle and its kind
//! and size are recorded here so a loader can refuse a file that does not
//! match.

use core::{fmt, num::NonZeroU32};
use std::collections::BTreeMap;

use fm_grade::{
    CubeKind, Grade, LUT_URI_PREFIX, MAX_CUBE_1D_SIZE, MAX_CUBE_3D_EDGE, MIN_CUBE_SIZE,
};
use fm_types::{InputId, SceneId};

use crate::LayerId;

/// Most named grades one project library may hold.
pub const MAX_PROJECT_GRADES: usize = 256;
/// Most graded scene layers one project may hold.
pub const MAX_PROJECT_LAYER_GRADES: usize = 256;
/// Most imported LUTs one project may hold.
pub const MAX_PROJECT_LUTS: usize = 64;
/// Longest grade name, in UTF-8 bytes.
pub const MAX_GRADE_NAME_BYTES: usize = 128;
/// Longest LUT display name, in UTF-8 bytes.
pub const MAX_LUT_NAME_BYTES: usize = 128;
/// Largest `.cube` file a project imports or loads, in bytes.
pub const MAX_LUT_FILE_BYTES: u64 = fm_grade::MAX_CUBE_INPUT_BYTES as u64;

/// Project-local identity of a library grade.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GradeId(NonZeroU32);

impl GradeId {
    #[must_use]
    pub const fn new(value: NonZeroU32) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> NonZeroU32 {
        self.0
    }
}

impl fmt::Display for GradeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Project-local identity of an imported LUT; it also names the bundled file.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LutId(NonZeroU32);

impl LutId {
    #[must_use]
    pub const fn new(value: NonZeroU32) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> NonZeroU32 {
        self.0
    }
}

impl fmt::Display for LutId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// The asset key of a LUT's bundled file, relative to `assets/`.
#[must_use]
pub fn lut_asset_key(id: LutId) -> String {
    format!("luts/{id}.cube")
}

/// The exact URI a grade uses to reference LUT `id`.
#[must_use]
pub fn lut_asset_uri(id: LutId) -> String {
    format!("{LUT_URI_PREFIX}{id}.cube")
}

/// The LUT a canonical LUT URI names, or `None` for any other URI.
#[must_use]
pub fn lut_id_from_uri(uri: &str) -> Option<LutId> {
    let digits = uri.strip_prefix(LUT_URI_PREFIX)?.strip_suffix(".cube")?;
    if digits.is_empty() || digits.starts_with('0') || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<NonZeroU32>().ok().map(LutId::new)
}

/// Allocates a library grade or LUT id: the smallest id above the highest of
/// `used` that `taken` does not claim, wrapping to the lowest id that is
/// neither used nor taken once every id above the highest is (a client may
/// name any id, `u32::MAX` included). `taken` adds ids that must not be
/// reused although nothing uses them now, such as a removed LUT whose
/// bundled file older manifests still name. `None` only when every id is
/// claimed.
#[must_use]
pub fn next_free_id(
    used: impl IntoIterator<Item = NonZeroU32>,
    mut taken: impl FnMut(NonZeroU32) -> bool,
) -> Option<NonZeroU32> {
    let used: std::collections::BTreeSet<u32> = used.into_iter().map(NonZeroU32::get).collect();
    let highest = used.last().copied().unwrap_or(0);
    highest
        .checked_add(1)
        .into_iter()
        .flat_map(|above| above..=u32::MAX)
        .chain(1..=highest)
        .filter(|id| !used.contains(id))
        .filter_map(NonZeroU32::new)
        .find(|id| !taken(*id))
}

/// A named grade in the project library.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectGrade {
    pub id: GradeId,
    pub name: String,
    pub grade: Grade,
}

/// An imported `.cube` LUT bundled at `assets/luts/<id>.cube`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectLut {
    pub id: LutId,
    pub name: String,
    pub kind: CubeKind,
    /// Edge length for a 3D LUT, entry count for a 1D LUT.
    pub size: u32,
}

impl ProjectLut {
    /// The URI grades use to reference this LUT.
    #[must_use]
    pub fn asset_uri(&self) -> String {
        lut_asset_uri(self.id)
    }

    /// Whether `size` is within the `.cube` bounds for `kind`.
    #[must_use]
    pub fn size_in_bounds(&self) -> bool {
        let size = usize::try_from(self.size).unwrap_or(usize::MAX);
        let maximum = match self.kind {
            CubeKind::OneD => MAX_CUBE_1D_SIZE,
            CubeKind::ThreeD => MAX_CUBE_3D_EDGE,
        };
        (MIN_CUBE_SIZE..=maximum).contains(&size)
    }
}

/// One layer of one scene: the target of a scene layer grade. Ordered by
/// scene, then layer.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SceneLayerRef {
    pub scene: SceneId,
    pub layer: LayerId,
}

impl SceneLayerRef {
    #[must_use]
    pub const fn new(scene: SceneId, layer: LayerId) -> Self {
        Self { scene, layer }
    }
}

impl fmt::Display for SceneLayerRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "scene {} layer {}", self.scene, self.layer)
    }
}

/// The grade one target (an input, a scene layer, or the program output)
/// carries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GradeSource {
    /// A grade from the project library.
    Library(GradeId),
    /// A grade owned by this target alone.
    Inline(Box<Grade>),
}

/// Every persisted grading value of a project.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProjectGrading {
    /// Library grades in authored order.
    pub grades: Vec<ProjectGrade>,
    /// Imported LUTs in import order.
    pub luts: Vec<ProjectLut>,
    /// Per-input grades, in input id order. An input without an entry is
    /// ungraded.
    pub inputs: BTreeMap<InputId, GradeSource>,
    /// Per-scene-layer grades, in scene then layer id order. A layer grade
    /// colours that layer after its source's input grade and its effects
    /// and before its transform. A layer without an entry is ungraded.
    pub layers: BTreeMap<SceneLayerRef, GradeSource>,
    /// The program output grade, applied to the composed Program picture.
    pub program: Option<GradeSource>,
}

impl ProjectGrading {
    #[must_use]
    pub fn grade(&self, id: GradeId) -> Option<&ProjectGrade> {
        self.grades.iter().find(|grade| grade.id == id)
    }

    #[must_use]
    pub fn lut(&self, id: LutId) -> Option<&ProjectLut> {
        self.luts.iter().find(|lut| lut.id == id)
    }

    /// The grade `source` evaluates to, or `None` for a dangling library id.
    #[must_use]
    pub fn resolve<'a>(&'a self, source: &'a GradeSource) -> Option<&'a Grade> {
        match source {
            GradeSource::Library(id) => self.grade(*id).map(|entry| &entry.grade),
            GradeSource::Inline(grade) => Some(grade),
        }
    }

    /// Every target grade: the input grades, the layer grades, then the
    /// program grade.
    pub fn targets(&self) -> impl Iterator<Item = &GradeSource> {
        self.inputs
            .values()
            .chain(self.layers.values())
            .chain(self.program.iter())
    }

    /// Whether any library grade, input, scene layer, or the program
    /// references `lut`.
    #[must_use]
    pub fn lut_in_use(&self, lut: LutId) -> bool {
        let uses = |grade: &Grade| {
            grade
                .lut
                .as_ref()
                .is_some_and(|reference| lut_id_from_uri(&reference.asset_uri) == Some(lut))
        };
        self.grades.iter().any(|entry| uses(&entry.grade))
            || self
                .targets()
                .any(|source| matches!(source, GradeSource::Inline(grade) if uses(grade)))
    }

    /// Whether any input, scene layer, or the program references library
    /// grade `id`.
    #[must_use]
    pub fn grade_in_use(&self, id: GradeId) -> bool {
        self.targets()
            .any(|source| *source == GradeSource::Library(id))
    }

    /// The LUTs referenced by a library grade, an input, a scene layer, or
    /// the program, in id order. Unregistered or non-canonical references
    /// are omitted.
    #[must_use]
    pub fn referenced_luts(&self) -> Vec<LutId> {
        let mut luts = self
            .grades
            .iter()
            .map(|entry| &entry.grade)
            .chain(self.targets().filter_map(|source| match source {
                GradeSource::Inline(grade) => Some(grade.as_ref()),
                GradeSource::Library(_) => None,
            }))
            .filter_map(|grade| grade.lut.as_ref())
            .filter_map(|lut| lut_id_from_uri(&lut.asset_uri))
            .filter(|id| self.lut(*id).is_some())
            .collect::<Vec<_>>();
        luts.sort_unstable();
        luts.dedup();
        luts
    }
}

impl ProjectGrading {
    /// Inserts a named grade into the library at `index` (the end when
    /// `None`).
    ///
    /// # Errors
    ///
    /// Refuses a full library, a used id or name, an invalid name, an index
    /// past the end, and a grade that violates a bound or names a LUT that
    /// was not imported.
    pub fn insert_grade(
        &mut self,
        index: Option<usize>,
        grade: ProjectGrade,
    ) -> Result<(), GradeEditError> {
        if self.grades.len() >= MAX_PROJECT_GRADES {
            return Err(GradeEditError::TooManyGrades {
                maximum: MAX_PROJECT_GRADES,
            });
        }
        if self.grade(grade.id).is_some() {
            return Err(GradeEditError::DuplicateGradeId(grade.id));
        }
        if let Some(error) = grade_name_error(&grade.name, MAX_GRADE_NAME_BYTES) {
            return Err(error);
        }
        if self
            .grades
            .iter()
            .any(|candidate| candidate.name == grade.name)
        {
            return Err(GradeEditError::DuplicateName(grade.name));
        }
        check_grade(&grade.grade, &self.luts)?;
        let index = index.unwrap_or(self.grades.len());
        if index > self.grades.len() {
            return Err(GradeEditError::IndexOutOfRange {
                index,
                len: self.grades.len(),
            });
        }
        self.grades.insert(index, grade);
        Ok(())
    }

    /// Renames a library grade and replaces its values; every target
    /// referencing it follows.
    ///
    /// # Errors
    ///
    /// Refuses an unknown id, an invalid name or one another grade uses, and
    /// an invalid grade.
    pub fn update_grade(
        &mut self,
        id: GradeId,
        name: String,
        grade: Grade,
    ) -> Result<(), GradeEditError> {
        if self.grade(id).is_none() {
            return Err(GradeEditError::UnknownGrade(id));
        }
        if let Some(error) = grade_name_error(&name, MAX_GRADE_NAME_BYTES) {
            return Err(error);
        }
        if self
            .grades
            .iter()
            .any(|candidate| candidate.id != id && candidate.name == name)
        {
            return Err(GradeEditError::DuplicateName(name));
        }
        check_grade(&grade, &self.luts)?;
        let entry = self
            .grades
            .iter_mut()
            .find(|candidate| candidate.id == id)
            .ok_or(GradeEditError::UnknownGrade(id))?;
        entry.name = name;
        entry.grade = grade;
        Ok(())
    }

    /// Removes a library grade no input or program references and returns
    /// its former index with it.
    ///
    /// # Errors
    ///
    /// Refuses an unknown id and a grade that is still assigned.
    pub fn remove_grade(&mut self, id: GradeId) -> Result<(usize, ProjectGrade), GradeEditError> {
        let index = self
            .grades
            .iter()
            .position(|candidate| candidate.id == id)
            .ok_or(GradeEditError::UnknownGrade(id))?;
        if self.grade_in_use(id) {
            return Err(GradeEditError::GradeInUse(id));
        }
        Ok((index, self.grades.remove(index)))
    }

    /// Registers an imported LUT after the others.
    ///
    /// # Errors
    ///
    /// Refuses a full LUT table, a used id or name, an invalid name, and a
    /// size outside the `.cube` bounds for its kind.
    pub fn add_lut(&mut self, lut: ProjectLut) -> Result<(), GradeEditError> {
        if self.luts.len() >= MAX_PROJECT_LUTS {
            return Err(GradeEditError::TooManyLuts {
                maximum: MAX_PROJECT_LUTS,
            });
        }
        if self.lut(lut.id).is_some() {
            return Err(GradeEditError::DuplicateLutId(lut.id));
        }
        if let Some(error) = grade_name_error(&lut.name, MAX_LUT_NAME_BYTES) {
            return Err(error);
        }
        if self.luts.iter().any(|candidate| candidate.name == lut.name) {
            return Err(GradeEditError::DuplicateName(lut.name));
        }
        if !lut.size_in_bounds() {
            return Err(GradeEditError::LutSizeOutOfRange {
                kind: lut.kind,
                size: lut.size,
            });
        }
        self.luts.push(lut);
        Ok(())
    }

    /// Unregisters a LUT no grade references.
    ///
    /// # Errors
    ///
    /// Refuses an unknown id and a LUT that is still referenced.
    pub fn remove_lut(&mut self, id: LutId) -> Result<ProjectLut, GradeEditError> {
        let index = self
            .luts
            .iter()
            .position(|candidate| candidate.id == id)
            .ok_or(GradeEditError::UnknownLut(id))?;
        if self.lut_in_use(id) {
            return Err(GradeEditError::LutInUse(id));
        }
        Ok(self.luts.remove(index))
    }

    /// Sets or clears one input's grade without checking that the input
    /// exists (the caller owns the input catalogue).
    ///
    /// # Errors
    ///
    /// Refuses an unknown library grade and an invalid inline grade.
    pub fn set_input(
        &mut self,
        input: InputId,
        grade: Option<GradeSource>,
    ) -> Result<(), GradeEditError> {
        match grade {
            Some(source) => {
                self.check_source(&source)?;
                self.inputs.insert(input, source);
            }
            None => {
                self.inputs.remove(&input);
            }
        }
        Ok(())
    }

    /// Sets or clears one scene layer's grade without checking that the
    /// scene and layer exist (the caller owns the scenes).
    ///
    /// # Errors
    ///
    /// Refuses an unknown library grade, an invalid inline grade, and a new
    /// graded layer beyond [`MAX_PROJECT_LAYER_GRADES`].
    pub fn set_layer(
        &mut self,
        layer: SceneLayerRef,
        grade: Option<GradeSource>,
    ) -> Result<(), GradeEditError> {
        match grade {
            Some(source) => {
                self.check_source(&source)?;
                if !self.layers.contains_key(&layer)
                    && self.layers.len() >= MAX_PROJECT_LAYER_GRADES
                {
                    return Err(GradeEditError::TooManyLayerGrades {
                        maximum: MAX_PROJECT_LAYER_GRADES,
                    });
                }
                self.layers.insert(layer, source);
            }
            None => {
                self.layers.remove(&layer);
            }
        }
        Ok(())
    }

    /// Drops every layer grade of `scene`.
    pub fn remove_scene_layers(&mut self, scene: SceneId) {
        self.layers.retain(|target, _| target.scene != scene);
    }

    /// Sets or clears the program output grade.
    ///
    /// # Errors
    ///
    /// Refuses an unknown library grade and an invalid inline grade.
    pub fn set_program(&mut self, grade: Option<GradeSource>) -> Result<(), GradeEditError> {
        if let Some(source) = &grade {
            self.check_source(source)?;
        }
        self.program = grade;
        Ok(())
    }

    /// Checks that `source` names a library grade or is a valid inline
    /// grade whose LUT was imported.
    ///
    /// # Errors
    ///
    /// Refuses an unknown library grade and an invalid inline grade.
    pub fn check_source(&self, source: &GradeSource) -> Result<(), GradeEditError> {
        match source {
            GradeSource::Library(id) => self
                .grade(*id)
                .map(|_| ())
                .ok_or(GradeEditError::UnknownGrade(*id)),
            GradeSource::Inline(grade) => check_grade(grade, &self.luts),
        }
    }
}

/// Checks a grade or LUT name: nonblank, bounded, and free of control
/// characters.
pub(crate) fn grade_name_error(name: &str, maximum: usize) -> Option<GradeEditError> {
    if name.trim().is_empty() {
        Some(GradeEditError::EmptyName)
    } else if name.len() > maximum {
        Some(GradeEditError::NameTooLong {
            bytes: name.len(),
            maximum,
        })
    } else if name.chars().any(char::is_control) {
        Some(GradeEditError::NameControlCharacter)
    } else {
        None
    }
}

/// A refused grading edit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GradeEditError {
    UnknownInput(InputId),
    /// The scene does not exist, or (for an engine edit) no Scene input
    /// shows it.
    UnknownScene(SceneId),
    /// The scene has no layer with this id.
    UnknownLayer(SceneLayerRef),
    UnknownGrade(GradeId),
    UnknownLut(LutId),
    DuplicateGradeId(GradeId),
    DuplicateLutId(LutId),
    DuplicateName(String),
    EmptyName,
    NameTooLong {
        bytes: usize,
        maximum: usize,
    },
    NameControlCharacter,
    TooManyGrades {
        maximum: usize,
    },
    TooManyLuts {
        maximum: usize,
    },
    TooManyLayerGrades {
        maximum: usize,
    },
    LutSizeOutOfRange {
        kind: CubeKind,
        size: u32,
    },
    /// The grade violates an `fm_grade` bound.
    InvalidGrade(fm_grade::GradeError),
    /// The grade's LUT URI is not `asset://luts/<id>.cube` of an imported LUT.
    UnregisteredLut(String),
    GradeInUse(GradeId),
    LutInUse(LutId),
    /// An insertion index past the end of its list.
    IndexOutOfRange {
        index: usize,
        len: usize,
    },
}

impl fmt::Display for GradeEditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownInput(input) => write!(formatter, "input {input} does not exist"),
            Self::UnknownScene(scene) => write!(formatter, "scene {scene} does not exist"),
            Self::UnknownLayer(target) => write!(formatter, "{target} does not exist"),
            Self::UnknownGrade(id) => write!(formatter, "grade {id} does not exist"),
            Self::UnknownLut(id) => write!(formatter, "LUT {id} does not exist"),
            Self::DuplicateGradeId(id) => write!(formatter, "grade id {id} is already used"),
            Self::DuplicateLutId(id) => write!(formatter, "LUT id {id} is already used"),
            Self::DuplicateName(name) => write!(formatter, "name `{name}` is already used"),
            Self::EmptyName => formatter.write_str("name must not be blank"),
            Self::NameTooLong { bytes, maximum } => {
                write!(formatter, "name is {bytes} bytes; the maximum is {maximum}")
            }
            Self::NameControlCharacter => {
                formatter.write_str("name must not contain control characters")
            }
            Self::TooManyGrades { maximum } => {
                write!(
                    formatter,
                    "the grade library holds at most {maximum} grades"
                )
            }
            Self::TooManyLuts { maximum } => {
                write!(formatter, "a project holds at most {maximum} LUTs")
            }
            Self::TooManyLayerGrades { maximum } => {
                write!(formatter, "a project grades at most {maximum} scene layers")
            }
            Self::LutSizeOutOfRange { kind, size } => {
                write!(formatter, "{kind:?} LUT size {size} is out of range")
            }
            Self::InvalidGrade(error) => write!(formatter, "invalid grade: {error}"),
            Self::UnregisteredLut(uri) => {
                write!(formatter, "grade LUT `{uri}` is not an imported LUT")
            }
            Self::GradeInUse(id) => write!(formatter, "grade {id} is still assigned"),
            Self::LutInUse(id) => write!(formatter, "LUT {id} is still used by a grade"),
            Self::IndexOutOfRange { index, len } => {
                write!(formatter, "index {index} is past the end of {len} entries")
            }
        }
    }
}

impl std::error::Error for GradeEditError {}

/// Validates one grade's bounds and LUT reference against `luts`.
pub(crate) fn check_grade(grade: &Grade, luts: &[ProjectLut]) -> Result<(), GradeEditError> {
    grade.validate().map_err(GradeEditError::InvalidGrade)?;
    if let Some(lut) = &grade.lut {
        let registered = lut_id_from_uri(&lut.asset_uri)
            .is_some_and(|id| luts.iter().any(|candidate| candidate.id == id));
        if !registered {
            return Err(GradeEditError::UnregisteredLut(lut.asset_uri.clone()));
        }
    }
    Ok(())
}
