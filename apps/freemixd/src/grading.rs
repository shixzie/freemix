//! Project grading in the daemon (SW-019): LUTs loaded from the bundle at
//! startup, LUT files written when an import becomes durable, and the
//! compiled per-input and program grades the simulated and native renderers
//! apply.
//!
//! Grades are engine state ([`fm_engine::EngineGradeEdit`]), so they are
//! journaled, undone, and persisted like any project edit. The engine holds
//! each imported LUT's exact text; the bundle file `assets/luts/<id>.cube`
//! is written before the import's journal batch, so every manifest or
//! journal that names a LUT finds its file.

use std::collections::BTreeMap;
use std::sync::Arc;

use fm_auth::Policy;
use fm_control::ControlService;
use fm_engine::{CompiledProjectGrades, EngineLut};
use fm_model::{LutId, Project};
use fm_persistence::ProjectStore;

/// Every imported LUT of `project` whose bundled file loads and matches its
/// record. A LUT that does not load is reported on stderr and left out: its
/// grades bypass the LUT stage (and a journal replay that re-imports it
/// restores it), exactly as a missing LUT renders offline.
pub(crate) fn load_grade_luts(
    store: &ProjectStore,
    project: &Project,
) -> BTreeMap<LutId, EngineLut> {
    project
        .luts()
        .iter()
        .filter_map(|lut| match store.load_lut_source(lut) {
            Ok((source, cube)) => Some((lut.id, EngineLut::from_parsed(source, cube))),
            Err(error) => {
                eprintln!(
                    "FREEMIXD_GRADING\tv=1\tlut={}\tstatus=bypassed\terror={error}",
                    lut.id
                );
                None
            }
        })
        .collect()
}

/// Compiled grades of one show, cached per durable revision.
#[derive(Default)]
pub(crate) struct GradeCache {
    compiled: Option<(u64, Arc<CompiledProjectGrades>)>,
}

impl GradeCache {
    /// The compiled grades of `control`'s engine at its durable revision,
    /// compiling only when the revision changed, and the same shared grades
    /// as before when the compiled result did not change. A grade that cannot compile (never produced by an
    /// accepted edit) renders ungraded.
    pub(crate) fn at(&mut self, control: &ControlService<Policy>) -> Arc<CompiledProjectGrades> {
        let revision = control.snapshot().snapshot.revision;
        if let Some((cached, grades)) = &self.compiled
            && *cached == revision
        {
            return Arc::clone(grades);
        }
        let compiled = control.compiled_grades().unwrap_or_else(|error| {
            eprintln!("FREEMIXD_GRADING\tv=1\tstatus=ungraded\terror={error}");
            CompiledProjectGrades::default()
        });
        // Most revisions change no grade: keep sharing the grades they left,
        // so consumers that key baked or uploaded work on the shared
        // grades (`Arc::ptr_eq`) redo it only when a grade changed.
        let grades = match self.compiled.take() {
            Some((_, previous)) if *previous == compiled => previous,
            _ => Arc::new(compiled),
        };
        self.compiled = Some((revision, Arc::clone(&grades)));
        grades
    }
}
