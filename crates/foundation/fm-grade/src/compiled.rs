use core::fmt;
use std::sync::Arc;

use crate::cube::CubeLut;
use crate::model::{Grade, GradeError};
use crate::reference::{Prepared, active_lut, clamp01, normalize, quantize};

/// Smallest [`CompiledGrade::bake`] edge.
pub const MIN_BAKE_EDGE: usize = 2;
/// Largest [`CompiledGrade::bake`] edge.
pub const MAX_BAKE_EDGE: usize = 65;

const CODES: usize = 256;

/// A validated grade prepared for fast, exact evaluation.
///
/// Stages 1–4 are per channel and stored as three 256-entry `f32` tables,
/// each entry equal to direct evaluation of that 8-bit code. When stages
/// 5–8 are neutral the full grade collapses to three 256-entry byte tables.
/// Output is bit-identical to [`crate::reference::evaluate_pixel`].
#[derive(Clone)]
pub struct CompiledGrade {
    grade: Grade,
    lut: Option<Arc<CubeLut>>,
    lut_strength: Option<f32>,
    identity: bool,
    prepared: Prepared,
    primary: Box<[[f32; CODES]; 3]>,
    direct: Option<Box<[[u8; CODES]; 3]>>,
}

impl CompiledGrade {
    /// Validates and compiles `grade`.
    ///
    /// `lut` is the parsed asset for `grade.lut`; it is ignored when the
    /// grade references no LUT. When the grade references a LUT with
    /// nonzero strength but `lut` is `None`, the LUT stage is bypassed and
    /// [`Self::lut_missing`] reports it.
    ///
    /// # Errors
    /// Returns the first violated [`Grade`] bound.
    pub fn new(grade: &Grade, lut: Option<Arc<CubeLut>>) -> Result<Self, GradeError> {
        grade.validate()?;
        let prepared = Prepared::new(grade);
        let lut_strength = active_lut(grade, lut.as_deref()).map(|(_, strength)| strength);
        let identity = grade.is_identity()
            || (prepared.primary_is_neutral()
                && prepared.secondary_is_neutral()
                && lut_strength.is_none());
        let mut primary = Box::new([[0.0_f32; CODES]; 3]);
        for code in 0..=u8::MAX {
            let graded = prepared.primary([normalize(code); 3]);
            for (channel, table) in primary.iter_mut().enumerate() {
                table[usize::from(code)] = graded[channel];
            }
        }
        let direct = (prepared.secondary_is_neutral() && lut_strength.is_none()).then(|| {
            let mut direct = Box::new([[0_u8; CODES]; 3]);
            for (channel, table) in direct.iter_mut().enumerate() {
                for (code, value) in table.iter_mut().enumerate() {
                    *value = quantize(primary[channel][code]);
                }
            }
            direct
        });
        Ok(Self {
            grade: grade.clone(),
            lut,
            lut_strength,
            identity,
            prepared,
            primary,
            direct,
        })
    }

    /// The exact identity grade.
    #[must_use]
    pub fn identity() -> Self {
        match Self::new(&Grade::IDENTITY, None) {
            Ok(grade) => grade,
            Err(_) => unreachable!("the identity grade is valid"),
        }
    }

    #[must_use]
    pub const fn grade(&self) -> &Grade {
        &self.grade
    }

    #[must_use]
    pub fn lut(&self) -> Option<&Arc<CubeLut>> {
        self.lut.as_ref()
    }

    /// True when evaluation returns source bytes exactly.
    #[must_use]
    pub const fn is_identity(&self) -> bool {
        self.identity
    }

    /// True when the grade references a LUT with nonzero strength that was
    /// not supplied, so the LUT stage is bypassed.
    #[must_use]
    pub fn lut_missing(&self) -> bool {
        self.grade.enabled
            && self.lut_strength.is_none()
            && self
                .grade
                .lut
                .as_ref()
                .is_some_and(|lut| lut.strength_permille > 0)
    }

    /// Stages 1–4 per channel (`[red, green, blue][code]`).
    #[must_use]
    pub fn primary_tables(&self) -> &[[f32; CODES]; 3] {
        &self.primary
    }

    /// Grades one straight 8-bit RGB pixel.
    #[must_use]
    pub fn apply_rgb8(&self, rgb: [u8; 3]) -> [u8; 3] {
        if self.identity {
            return rgb;
        }
        if let Some(direct) = &self.direct {
            return core::array::from_fn(|channel| direct[channel][usize::from(rgb[channel])]);
        }
        let primary: [f32; 3] =
            core::array::from_fn(|channel| self.primary[channel][usize::from(rgb[channel])]);
        self.prepared
            .secondary(primary, self.active_lut())
            .map(quantize)
    }

    /// Grades packed straight RGBA8 pixels in place; alpha is untouched and
    /// a trailing partial pixel is ignored.
    pub fn apply_rgba8_in_place(&self, pixels: &mut [u8]) {
        if self.identity {
            return;
        }
        let (pixels, _) = pixels.as_chunks_mut::<4>();
        for pixel in pixels {
            let graded = self.apply_rgb8([pixel[0], pixel[1], pixel[2]]);
            pixel[..3].copy_from_slice(&graded);
        }
    }

    /// The full continuous grade (stages 2–8 then `clamp01`) at `rgb`.
    #[must_use]
    pub fn evaluate(&self, rgb: [f32; 3]) -> [f32; 3] {
        if self.identity {
            return clamp01(rgb);
        }
        clamp01(
            self.prepared
                .secondary(self.prepared.primary(rgb), self.active_lut()),
        )
    }

    /// Samples [`Self::evaluate`] on a red-fastest `edge³` grid over
    /// `[0, 1]³` (`index = (blue * edge + green) * edge + red`), suitable for
    /// a 3D LUT texture. `edge` is clamped to `2..=65`.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn bake(&self, edge: usize) -> Vec<[f32; 3]> {
        let edge = edge.clamp(MIN_BAKE_EDGE, MAX_BAKE_EDGE);
        // Edges are at most 65, so every index converts to f32 exactly.
        let coordinate = |index: usize| index as f32 / (edge - 1) as f32;
        let mut grid = Vec::with_capacity(edge * edge * edge);
        for blue in 0..edge {
            for green in 0..edge {
                for red in 0..edge {
                    grid.push(self.evaluate([
                        coordinate(red),
                        coordinate(green),
                        coordinate(blue),
                    ]));
                }
            }
        }
        grid
    }

    fn active_lut(&self) -> Option<(&CubeLut, f32)> {
        self.lut.as_deref().zip(self.lut_strength)
    }
}

/// Compiled grades are equal when built from equal grades and LUTs; all
/// derived tables are deterministic functions of those inputs.
impl PartialEq for CompiledGrade {
    fn eq(&self, other: &Self) -> bool {
        self.grade == other.grade
            && match (&self.lut, &other.lut) {
                (Some(left), Some(right)) => Arc::ptr_eq(left, right) || left == right,
                (None, None) => true,
                _ => false,
            }
    }
}

impl Eq for CompiledGrade {}

impl fmt::Debug for CompiledGrade {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompiledGrade")
            .field("grade", &self.grade)
            .field(
                "lut",
                &self.lut.as_ref().map(|lut| (lut.kind(), lut.size())),
            )
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}
