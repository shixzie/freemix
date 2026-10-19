//! Binds one [`GradeDocument`] to the replicated grade of one target (an
//! input, a scene layer, or the program) and turns draft edits into grade
//! commands.
//!
//! The authority owns the grade: [`GradeBinding::sync`] adopts every
//! authority update while the draft is clean, and keeps a dirty draft (an
//! edit not yet confirmed, or one the server refused) until the authority
//! catches up with it or the operator reverts. [`GradeBinding::poll`] sends
//! the draft: at most every [`DRAG_SEND_INTERVAL`] while a drag is in
//! progress (latest wins), and immediately once it is released; every set
//! of one drag carries the drag's gesture token (Protocol 2.56) so the
//! server folds the drag into one undo step. Clients
//! apply no optimistic grade change (Protocol 2.28), so the draft is the
//! only optimistic picture and is never mistaken for authority state.

use core::num::NonZeroU32;
use core::time::Duration;

use fm_grade::{Grade, LUT_URI_PREFIX};
use fm_protocol::{
    CommandPayload, GradeGesture, MAX_GRADE_LUT_IMPORT_BYTES, MAX_GRADE_NAME_BYTES,
    WireGradeSource, WireInputId, WireSceneId, WireSceneLayerId,
};
use fm_types::{InputId, SceneId};

use super::document::{GradeDocument, GradeEditorError};
use crate::GradingStatus;

fn fresh_token() -> core::num::NonZeroU64 {
    core::num::NonZeroU64::new(crate::scene_designer::document::fresh_gesture())
        .expect("gesture tokens are nonzero")
}

/// Shortest gap between two grade commands sent during one drag.
pub const DRAG_SEND_INTERVAL: Duration = Duration::from_millis(120);

/// What a grade applies to.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum GradeTarget {
    Input(InputId),
    /// One layer of scene `scene` (Protocol 2.35), whether or not a Scene
    /// input shows it. The grade belongs to the scene: every Scene input
    /// showing it shares it.
    Layer {
        scene: SceneId,
        layer: WireSceneLayerId,
    },
    /// The composed Program picture.
    Program,
}

/// The authority's grade source for `target`, if graded.
#[must_use]
pub fn target_source(grading: &GradingStatus, target: GradeTarget) -> Option<&WireGradeSource> {
    match target {
        GradeTarget::Input(input) => grading.inputs.get(&input),
        GradeTarget::Layer { scene, layer } => grading.layer(scene, layer),
        GradeTarget::Program => grading.program.as_ref(),
    }
}

/// The grade the authority applies to `target`: its resolved source, or
/// [`Grade::IDENTITY`] when ungraded (or naming an unknown library grade).
#[must_use]
pub fn target_grade(grading: &GradingStatus, target: GradeTarget) -> Grade {
    target_source(grading, target)
        .and_then(|source| grading.resolve(source))
        .cloned()
        .unwrap_or(Grade::IDENTITY)
}

/// The `asset://` URI an imported LUT is referenced by.
#[must_use]
pub fn lut_uri(lut: NonZeroU32) -> String {
    format!("{LUT_URI_PREFIX}{lut}.cube")
}

/// The imported LUT `uri` names, if it is one.
#[must_use]
pub fn lut_id(uri: &str) -> Option<NonZeroU32> {
    let id = uri.strip_prefix(LUT_URI_PREFIX)?.strip_suffix(".cube")?;
    // Canonical decimals only, as on the wire.
    if id.starts_with('0') || !id.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    id.parse().ok()
}

/// The smallest library grade id not in use.
#[must_use]
pub fn next_library_id(grading: &GradingStatus) -> Option<NonZeroU32> {
    first_free(grading.library.iter().map(|entry| entry.grade))
}

/// The id to import the next LUT under: never one the project used before.
///
/// A removed LUT's bundled file stays (older manifests and autosaves name
/// it) and the daemon refuses an import that would replace it, so an id is
/// not reused: allocation goes above the highest registered id, and past
/// `issued`, the last id this client sent (an import the daemon refused
/// moves on rather than repeating the same id). After `u32::MAX` it wraps to
/// the lowest unregistered id; `None` only when every id is registered.
#[must_use]
pub fn next_lut_id(grading: &GradingStatus, issued: Option<NonZeroU32>) -> Option<NonZeroU32> {
    let registered = |id: u32| grading.luts.iter().any(|entry| entry.lut.get() == id);
    let highest = grading
        .luts
        .iter()
        .map(|entry| entry.lut.get())
        .max()
        .unwrap_or(0);
    let start = match issued {
        Some(issued) if issued.get() > highest || highest == u32::MAX => issued.get(),
        _ => highest,
    };
    start
        .checked_add(1)
        .into_iter()
        .flat_map(|above| above..=u32::MAX)
        .chain(1..=start)
        .filter(|id| !registered(*id))
        .find_map(NonZeroU32::new)
}

fn first_free(used: impl Iterator<Item = NonZeroU32>) -> Option<NonZeroU32> {
    let mut used: Vec<u32> = used.map(NonZeroU32::get).collect();
    used.sort_unstable();
    let mut candidate = 1_u32;
    for id in used {
        if id == candidate {
            candidate = candidate.checked_add(1)?;
        } else if id > candidate {
            break;
        }
    }
    NonZeroU32::new(candidate)
}

/// Why a library grade or LUT name cannot be sent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GradeNameProblem {
    Blank,
    TooLong,
    ControlCharacter,
    /// Another library grade (or LUT) already has this name.
    Taken,
}

fn name_problem<'a>(
    name: &str,
    mut existing: impl Iterator<Item = &'a str>,
) -> Option<GradeNameProblem> {
    if name.trim().is_empty() {
        Some(GradeNameProblem::Blank)
    } else if name.len() > MAX_GRADE_NAME_BYTES {
        Some(GradeNameProblem::TooLong)
    } else if name.chars().any(char::is_control) {
        Some(GradeNameProblem::ControlCharacter)
    } else if existing.any(|other| other == name) {
        Some(GradeNameProblem::Taken)
    } else {
        None
    }
}

/// Why `name` cannot name a new library grade, if it cannot.
#[must_use]
pub fn library_name_problem(name: &str, grading: &GradingStatus) -> Option<GradeNameProblem> {
    name_problem(
        name,
        grading.library.iter().map(|entry| entry.name.as_str()),
    )
}

/// Why `name` cannot name a newly imported LUT, if it cannot.
#[must_use]
pub fn lut_name_problem(name: &str, grading: &GradingStatus) -> Option<GradeNameProblem> {
    name_problem(name, grading.luts.iter().map(|entry| entry.name.as_str()))
}

/// Whether `bytes` of `.cube` text fit one `grade_lut_import` command.
#[must_use]
pub const fn lut_fits_protocol(bytes: u64) -> bool {
    bytes <= MAX_GRADE_LUT_IMPORT_BYTES as u64
}

/// One grade command a Studio panel sends.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GradeCommand {
    /// Sets (`Some`) or clears (`None`) the grade of `target`.
    Set {
        target: GradeTarget,
        source: Option<WireGradeSource>,
        /// The drag the set belongs to (Protocol 2.56): the server folds a
        /// whole drag into one undo step.
        gesture: Option<GradeGesture>,
    },
    /// Saves `value` as a new library grade `grade`, appended.
    AddLibrary {
        grade: NonZeroU32,
        name: String,
        value: Box<Grade>,
    },
    /// Imports `.cube` text as LUT `lut`.
    ImportLut {
        lut: NonZeroU32,
        name: String,
        data: String,
    },
}

impl GradeCommand {
    /// The target a [`Self::Set`] grades.
    #[must_use]
    pub const fn target(&self) -> Option<GradeTarget> {
        match self {
            Self::Set { target, .. } => Some(*target),
            Self::AddLibrary { .. } | Self::ImportLut { .. } => None,
        }
    }

    /// The protocol payload.
    #[must_use]
    pub fn payload(&self) -> CommandPayload {
        match self.clone() {
            Self::Set {
                target: GradeTarget::Input(input),
                source,
                gesture,
            } => CommandPayload::SetInputGrade {
                input: WireInputId::from_domain(input),
                source,
                gesture,
            },
            Self::Set {
                target: GradeTarget::Layer { scene, layer },
                source,
                gesture,
            } => CommandPayload::SetLayerGrade {
                scene: WireSceneId::from_domain(scene),
                layer,
                source,
                gesture,
            },
            Self::Set {
                target: GradeTarget::Program,
                source,
                gesture,
            } => CommandPayload::SetProgramGrade { source, gesture },
            Self::AddLibrary { grade, name, value } => CommandPayload::AddLibraryGrade {
                grade,
                name,
                value,
                index: None,
            },
            Self::ImportLut { lut, name, data } => {
                CommandPayload::ImportGradeLut { lut, name, data }
            }
        }
    }

    /// The grade command `payload` carries, if any.
    #[must_use]
    pub fn from_payload(payload: &CommandPayload) -> Option<Self> {
        Some(match payload.clone() {
            CommandPayload::SetInputGrade {
                input,
                source,
                gesture,
            } => Self::Set {
                target: GradeTarget::Input(input.to_domain()),
                source,
                gesture,
            },
            CommandPayload::SetLayerGrade {
                scene,
                layer,
                source,
                gesture,
            } => Self::Set {
                target: GradeTarget::Layer {
                    scene: scene.to_domain(),
                    layer,
                },
                source,
                gesture,
            },
            CommandPayload::SetProgramGrade { source, gesture } => Self::Set {
                target: GradeTarget::Program,
                source,
                gesture,
            },
            CommandPayload::AddLibraryGrade {
                grade, name, value, ..
            } => Self::AddLibrary { grade, name, value },
            CommandPayload::ImportGradeLut { lut, name, data } => {
                Self::ImportLut { lut, name, data }
            }
            _ => return None,
        })
    }
}

/// How the draft relates to the authority grade.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GradeSyncStatus {
    /// The draft is the grade on air.
    OnAir,
    /// The draft changed and is about to be sent (a drag is in progress or
    /// the send interval has not passed).
    Editing,
    /// The draft was sent; the authority has not reported it yet.
    Sending,
    /// The server refused the last draft sent; it is not on air.
    Refused,
    /// The draft differs from the grade on air and is not being sent (its
    /// command's answer was lost); revert or edit again.
    Diverged,
}

/// What [`GradeBinding::sync`] did with an authority update.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncOutcome {
    /// Nothing changed.
    Unchanged,
    /// The authority now has the draft: it is saved, history kept.
    Confirmed,
    /// The clean draft was replaced by the authority grade.
    Reset,
    /// The authority differs from a dirty draft, which is kept.
    KeptDraft,
}

/// One grade target's draft, bound to its replicated authority grade.
#[derive(Clone, Debug)]
pub struct GradeBinding {
    target: GradeTarget,
    document: GradeDocument,
    authority: Grade,
    revision: u64,
    /// The draft generation that needs no command: already sent or
    /// adopted from the authority.
    settled: u64,
    last_send: Option<Duration>,
    /// A library grade applied as is: sent by reference while the draft
    /// still equals its value.
    library: Option<(NonZeroU32, Grade)>,
    /// Send even an unchanged draft (a library grade applied to a target
    /// that already had its values inline).
    force_send: bool,
    /// A sent draft the authority has not reported yet.
    in_flight: bool,
    refused: bool,
    /// The token of the drag whose sets were sent, while it lasts: its
    /// later sets continue its server undo step.
    drag: Option<core::num::NonZeroU64>,
}

impl GradeBinding {
    /// A binding for `target` at the authority's `grading` and `revision`.
    #[must_use]
    pub fn new(target: GradeTarget, grading: &GradingStatus, revision: u64) -> Self {
        let authority = target_grade(grading, target);
        let document = GradeDocument::new(authority.clone(), revision);
        Self {
            target,
            settled: document.generation(),
            document,
            authority,
            revision,
            last_send: None,
            library: None,
            force_send: false,
            in_flight: false,
            refused: false,
            drag: None,
        }
    }

    #[must_use]
    pub const fn target(&self) -> GradeTarget {
        self.target
    }

    #[must_use]
    pub const fn document(&self) -> &GradeDocument {
        &self.document
    }

    /// The draft, for the editor widgets. Edits are sent by [`Self::poll`].
    pub const fn document_mut(&mut self) -> &mut GradeDocument {
        &mut self.document
    }

    /// The authority grade last seen by [`Self::sync`].
    #[must_use]
    pub const fn authority(&self) -> &Grade {
        &self.authority
    }

    /// Switches to `target`, starting from its authority grade. The
    /// document's clipboard survives so a copied grade can be pasted onto
    /// another target.
    pub fn retarget(&mut self, target: GradeTarget, grading: &GradingStatus, revision: u64) {
        if target == self.target {
            return;
        }
        let clipboard = self.document.clipboard().cloned();
        *self = Self::new(target, grading, revision);
        self.document.set_clipboard(clipboard);
    }

    /// Adopts the authority `grading` at `revision`: a draft equal to the
    /// authority grade is saved (keeping its undo history), a clean draft
    /// is replaced, and a dirty draft (pending, refused, or mid-drag) is
    /// kept.
    pub fn sync(&mut self, grading: &GradingStatus, revision: u64) -> SyncOutcome {
        self.authority = target_grade(grading, self.target);
        self.revision = revision;
        if self.document.grade() == &self.authority {
            if self.document.is_dirty() || self.refused || self.in_flight {
                self.document.mark_saved(revision);
                self.refused = false;
                self.in_flight = false;
                return SyncOutcome::Confirmed;
            }
            return SyncOutcome::Unchanged;
        }
        if self.document.is_dirty()
            || self.document.gesture_active()
            || self.in_flight
            || self.refused
            || self.unsent()
        {
            return SyncOutcome::KeptDraft;
        }
        self.document
            .reset_from_authority(self.authority.clone(), revision);
        self.library = None;
        self.settle();
        SyncOutcome::Reset
    }

    /// Replaces the draft with the authority grade, dropping a refused or
    /// unsent edit and the local history.
    pub fn revert(&mut self) {
        self.document
            .reset_from_authority(self.authority.clone(), self.revision);
        self.library = None;
        self.force_send = false;
        self.in_flight = false;
        self.refused = false;
        self.settle();
    }

    /// Records that the server refused the last command for this target.
    pub const fn refused(&mut self) {
        self.refused = true;
        self.in_flight = false;
    }

    /// Forgets a sent draft whose answer will not arrive (its command was
    /// never sent, or the connection dropped): the draft stays dirty.
    pub const fn abandon_in_flight(&mut self) {
        self.in_flight = false;
    }

    /// Makes library grade `grade` (with `value`) the draft; it is sent by
    /// reference, so the target follows later edits of the library grade.
    ///
    /// # Errors
    ///
    /// [`GradeEditorError::GestureActive`] during a drag, or an invalid
    /// value.
    pub fn apply_library(
        &mut self,
        grade: NonZeroU32,
        value: &Grade,
    ) -> Result<(), GradeEditorError> {
        self.document.edit(|draft| {
            draft.clone_from(value);
            Ok(())
        })?;
        self.library = Some((grade, value.clone()));
        self.force_send = true;
        Ok(())
    }

    fn settle(&mut self) {
        self.settled = self.document.generation();
        self.force_send = false;
    }

    /// Whether the draft has changes [`Self::poll`] has not sent.
    #[must_use]
    pub fn unsent(&self) -> bool {
        self.force_send || self.document.generation() != self.settled
    }

    /// When the next command may go out, while one waits for the drag
    /// interval: callers repaint then.
    #[must_use]
    pub fn next_send_in(&self, now: Duration) -> Option<Duration> {
        if !self.unsent() {
            return None;
        }
        Some(
            self.last_send
                .filter(|_| self.document.gesture_active())
                .map_or(Duration::ZERO, |sent| {
                    (sent + DRAG_SEND_INTERVAL).saturating_sub(now)
                }),
        )
    }

    /// The command that sends the draft, if one is due at `now`: during a
    /// drag at most every [`DRAG_SEND_INTERVAL`], otherwise immediately.
    /// `None` without unsent changes, or when `can_send` is false (the
    /// changes then stay unsent).
    pub fn poll(&mut self, now: Duration, can_send: bool) -> Option<GradeCommand> {
        let dragging = self.document.gesture_active();
        if !can_send || !self.unsent() {
            if !dragging && !self.unsent() {
                // The drag ended with everything sent.
                self.drag = None;
            }
            return None;
        }
        if self.document.gesture_active()
            && self
                .last_send
                .is_some_and(|sent| now.saturating_sub(sent) < DRAG_SEND_INTERVAL)
        {
            return None;
        }
        let draft = self.document.grade().clone();
        let library = self
            .library
            .as_ref()
            .filter(|(_, value)| *value == draft)
            .map(|(grade, _)| *grade);
        if library.is_none() {
            self.library = None;
        }
        let force = self.force_send;
        self.settle();
        // A drag's first set opens its server undo step; the rest, and the
        // set on release, continue it. Other edits are standalone.
        let gesture = if dragging || self.drag.is_some() {
            let continues = self.drag.is_some();
            let token = *self.drag.get_or_insert_with(fresh_token);
            Some(GradeGesture { token, continues })
        } else {
            None
        };
        if !dragging {
            self.drag = None;
        }
        if draft == self.authority && !force {
            // Undone back to the grade on air: nothing to send.
            if gesture.is_some_and(|gesture| !gesture.continues) && dragging {
                // Nothing sent yet: the drag opens its step later.
                self.drag = None;
            }
            return None;
        }
        self.last_send = Some(now);
        self.in_flight = true;
        self.refused = false;
        let source = match library {
            Some(grade) => Some(WireGradeSource::Library(grade)),
            None if draft == Grade::IDENTITY => None,
            None => Some(WireGradeSource::Inline(Box::new(draft))),
        };
        Some(GradeCommand::Set {
            target: self.target,
            source,
            gesture,
        })
    }

    /// The grade source the draft gives another target: the linked library
    /// grade while the draft still equals it, otherwise its values inline
    /// (`None`, a clear, when neutral).
    #[must_use]
    pub fn copy_source(&self) -> Option<WireGradeSource> {
        let draft = self.document.grade();
        match &self.library {
            Some((grade, value)) if value == draft => Some(WireGradeSource::Library(*grade)),
            _ if *draft == Grade::IDENTITY => None,
            _ => Some(WireGradeSource::Inline(Box::new(draft.clone()))),
        }
    }

    /// The commands that copy the draft to `targets`: one standalone set
    /// (its own undo step) per target, in the order given, skipping this
    /// binding's own target, repeats, and targets that already carry
    /// exactly that source (`grading` is the replicated authority).
    #[must_use]
    pub fn copy_commands(
        &self,
        targets: &[GradeTarget],
        grading: &GradingStatus,
    ) -> Vec<GradeCommand> {
        let source = self.copy_source();
        let mut seen = Vec::with_capacity(targets.len());
        targets
            .iter()
            .copied()
            .filter(|target| {
                let fresh = *target != self.target && !seen.contains(target);
                seen.push(*target);
                fresh && target_source(grading, *target) != source.as_ref()
            })
            .map(|target| GradeCommand::Set {
                target,
                source: source.clone(),
                gesture: None,
            })
            .collect()
    }

    /// The token of the drag whose sets are being sent, while one lasts.
    #[must_use]
    pub const fn drag_token(&self) -> Option<core::num::NonZeroU64> {
        self.drag
    }

    /// How the draft relates to the authority grade now.
    #[must_use]
    pub fn status(&self) -> GradeSyncStatus {
        if self.refused {
            GradeSyncStatus::Refused
        } else if self.unsent() {
            GradeSyncStatus::Editing
        } else if self.document.grade() == &self.authority {
            GradeSyncStatus::OnAir
        } else if self.in_flight {
            GradeSyncStatus::Sending
        } else {
            GradeSyncStatus::Diverged
        }
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;

    use fm_grade::GradeLut;

    use super::*;
    use crate::LibraryGradeStatus;
    use crate::grade_editor::GradeParam;

    fn input(value: u128) -> InputId {
        InputId::new(NonZeroU128::new(value).unwrap())
    }

    fn id(value: u32) -> NonZeroU32 {
        NonZeroU32::new(value).unwrap()
    }

    fn graded(exposure: i16) -> Grade {
        let mut grade = Grade::IDENTITY;
        grade.exposure_millistops = exposure;
        grade
    }

    fn grading_with(target: GradeTarget, grade: Option<Grade>) -> GradingStatus {
        let mut grading = GradingStatus::default();
        let source = grade.map(|grade| WireGradeSource::Inline(Box::new(grade)));
        match target {
            GradeTarget::Input(input) => {
                if let Some(source) = source {
                    grading.inputs.insert(input, source);
                }
            }
            GradeTarget::Layer { scene, layer } => {
                if let Some(source) = source {
                    grading.layers.insert((scene, layer), source);
                }
            }
            GradeTarget::Program => grading.program = source,
        }
        grading
    }

    const MS: fn(u64) -> Duration = Duration::from_millis;

    #[test]
    fn clean_drafts_follow_the_authority_and_dirty_drafts_are_kept() {
        let target = GradeTarget::Input(input(2));
        let mut binding = GradeBinding::new(target, &GradingStatus::default(), 4);
        assert_eq!(binding.document().grade(), &Grade::IDENTITY);
        assert_eq!(binding.status(), GradeSyncStatus::OnAir);

        // Another client grades the input: the clean draft follows.
        let outcome = binding.sync(&grading_with(target, Some(graded(500))), 5);
        assert_eq!(outcome, SyncOutcome::Reset);
        assert_eq!(binding.document().grade(), &graded(500));
        assert_eq!(binding.document().base_revision(), 5);
        assert!(!binding.unsent(), "an adopted grade is not sent back");
        assert_eq!(binding.poll(MS(0), true), None);

        // A local edit makes the draft dirty; a further authority update
        // does not overwrite it.
        binding
            .document_mut()
            .set_param(GradeParam::Exposure, 900)
            .unwrap();
        assert_eq!(binding.status(), GradeSyncStatus::Editing);
        let outcome = binding.sync(&grading_with(target, Some(graded(-300))), 6);
        assert_eq!(outcome, SyncOutcome::KeptDraft);
        assert_eq!(binding.document().grade(), &graded(900));
        assert_eq!(binding.authority(), &graded(-300));

        // Once the authority has the draft it is saved, history kept.
        assert!(binding.poll(MS(10), true).is_some());
        assert_eq!(binding.status(), GradeSyncStatus::Sending);
        let outcome = binding.sync(&grading_with(target, Some(graded(900))), 7);
        assert_eq!(outcome, SyncOutcome::Confirmed);
        assert!(!binding.document().is_dirty());
        assert!(binding.document().can_undo());
        assert_eq!(binding.status(), GradeSyncStatus::OnAir);

        // Clean again, so the next authority update is adopted.
        assert_eq!(
            binding.sync(&grading_with(target, None), 8),
            SyncOutcome::Reset
        );
        assert_eq!(binding.document().grade(), &Grade::IDENTITY);
    }

    #[test]
    fn a_drag_keeps_its_draft_and_sends_on_an_interval_then_on_release() {
        let target = GradeTarget::Program;
        let mut binding = GradeBinding::new(target, &GradingStatus::default(), 1);
        binding.document_mut().begin_gesture().unwrap();
        let set = |binding: &mut GradeBinding, value: i16| {
            binding
                .document_mut()
                .update_gesture(|grade| {
                    grade.exposure_millistops = value;
                    Ok(())
                })
                .unwrap();
        };
        set(&mut binding, 100);
        let first = binding.poll(MS(1_000), true).expect("the first step sends");
        // The first set opens the drag's server undo step.
        let token = binding.drag_token().expect("a drag is being sent");
        assert_eq!(
            first,
            GradeCommand::Set {
                target,
                source: Some(WireGradeSource::Inline(Box::new(graded(100)))),
                gesture: Some(GradeGesture {
                    token,
                    continues: false,
                }),
            }
        );
        // Within the interval later steps wait; the latest one wins.
        set(&mut binding, 200);
        set(&mut binding, 300);
        assert_eq!(binding.poll(MS(1_050), true), None);
        assert_eq!(binding.next_send_in(MS(1_050)), Some(MS(70)));
        // The authority confirming an intermediate step leaves the drag.
        assert_eq!(
            binding.sync(&grading_with(target, Some(graded(100))), 2),
            SyncOutcome::KeptDraft
        );
        assert_eq!(binding.document().grade(), &graded(300));
        let second = binding.poll(MS(1_120), true).expect("the interval passed");
        assert_eq!(
            second.payload(),
            CommandPayload::SetProgramGrade {
                source: Some(WireGradeSource::Inline(Box::new(graded(300)))),
                gesture: Some(GradeGesture {
                    token,
                    continues: true,
                }),
            }
        );
        // Release commits immediately, without waiting for the interval,
        // and still continues the drag's step.
        set(&mut binding, 350);
        binding.document_mut().commit_gesture().unwrap();
        assert_eq!(binding.next_send_in(MS(1_130)), Some(Duration::ZERO));
        let last = binding.poll(MS(1_130), true).expect("release sends");
        assert_eq!(
            last,
            GradeCommand::Set {
                target,
                source: Some(WireGradeSource::Inline(Box::new(graded(350)))),
                gesture: Some(GradeGesture {
                    token,
                    continues: true,
                }),
            }
        );
        assert_eq!(binding.drag_token(), None, "the drag is over");
        assert_eq!(binding.poll(MS(1_131), true), None);
        assert_eq!(binding.document().undo_depth(), 1, "one drag, one step");

        // The next drag picks a fresh token; a click is standalone.
        binding.document_mut().begin_gesture().unwrap();
        set(&mut binding, 400);
        let next = binding.poll(MS(2_000), true).unwrap();
        let GradeCommand::Set {
            gesture: Some(next_gesture),
            ..
        } = next
        else {
            panic!("a drag set carries its gesture: {next:?}");
        };
        assert_ne!(next_gesture.token, token);
        assert!(!next_gesture.continues);
        binding.document_mut().commit_gesture().unwrap();
        assert_eq!(
            binding.poll(MS(2_001), true),
            None,
            "nothing new on release"
        );
        assert_eq!(binding.drag_token(), None);
        binding
            .document_mut()
            .set_param(GradeParam::Exposure, 0)
            .unwrap();
        assert!(matches!(
            binding.poll(MS(2_002), true),
            Some(GradeCommand::Set { gesture: None, .. })
        ));
    }

    #[test]
    fn commands_clear_neutral_grades_and_wait_for_permission() {
        let target = GradeTarget::Input(input(3));
        let mut binding = GradeBinding::new(target, &grading_with(target, Some(graded(400))), 3);
        binding
            .document_mut()
            .reset_param(GradeParam::Exposure)
            .unwrap();
        // Without permission the change stays unsent.
        assert_eq!(binding.poll(MS(0), false), None);
        assert!(binding.unsent());
        let command = binding.poll(MS(0), true).unwrap();
        assert_eq!(
            command.payload(),
            CommandPayload::SetInputGrade {
                input: WireInputId::from_domain(input(3)),
                source: None,
                gesture: None,
            }
        );
        assert_eq!(
            GradeCommand::from_payload(&command.payload()),
            Some(command)
        );
        // Undoing back to the grade on air sends nothing.
        binding.document_mut().undo().unwrap();
        assert_eq!(binding.poll(MS(5), true), None);
    }

    #[test]
    fn refusals_keep_the_draft_until_reverted_or_edited_again() {
        let target = GradeTarget::Input(input(1));
        let mut binding = GradeBinding::new(target, &GradingStatus::default(), 1);
        binding
            .document_mut()
            .set_param(GradeParam::Saturation, 1500)
            .unwrap();
        assert!(binding.poll(MS(0), true).is_some());
        binding.refused();
        assert_eq!(binding.status(), GradeSyncStatus::Refused);
        // The refusal leaves the authority unchanged; the draft stays.
        assert_eq!(
            binding.sync(&GradingStatus::default(), 2),
            SyncOutcome::KeptDraft
        );
        assert_eq!(binding.document().grade().saturation_permille, 1500);
        // Revert restores the grade on air.
        binding.revert();
        assert_eq!(binding.document().grade(), &Grade::IDENTITY);
        assert_eq!(binding.status(), GradeSyncStatus::OnAir);
        assert_eq!(binding.poll(MS(1), true), None);
    }

    #[test]
    fn library_grades_are_sent_by_reference_until_edited() {
        let target = GradeTarget::Program;
        let look = graded(-700);
        let mut grading = grading_with(target, Some(look.clone()));
        grading.library.push(LibraryGradeStatus {
            grade: id(4),
            name: "Night".into(),
            value: Box::new(look.clone()),
        });
        let mut binding = GradeBinding::new(target, &grading, 9);
        // Same values inline: applying still links the library grade.
        binding.apply_library(id(4), &look).unwrap();
        assert_eq!(
            binding.poll(MS(0), true),
            Some(GradeCommand::Set {
                target,
                source: Some(WireGradeSource::Library(id(4))),
                gesture: None,
            })
        );
        // An edit after applying sends inline values again.
        binding.apply_library(id(4), &look).unwrap();
        binding
            .document_mut()
            .set_param(GradeParam::Exposure, 0)
            .unwrap();
        assert_eq!(
            binding.poll(MS(10), true),
            Some(GradeCommand::Set {
                target,
                source: None,
                gesture: None,
            })
        );
    }

    #[test]
    fn copying_sends_the_draft_to_each_other_target_once() {
        let own = GradeTarget::Input(input(1));
        let layer = GradeTarget::Layer {
            scene: SceneId::new(core::num::NonZeroU128::new(7).unwrap()),
            layer: WireSceneLayerId::new(id(2)),
        };
        let mut grading = grading_with(GradeTarget::Input(input(3)), Some(graded(450)));
        let mut binding = GradeBinding::new(own, &grading, 1);
        binding
            .document_mut()
            .set_param(GradeParam::Exposure, 450)
            .unwrap();
        let inline = Some(WireGradeSource::Inline(Box::new(graded(450))));
        let targets = [
            own,
            GradeTarget::Input(input(2)),
            GradeTarget::Input(input(3)),
            layer,
            GradeTarget::Input(input(2)),
            GradeTarget::Program,
        ];
        // Not its own target, not twice, and not where it already is.
        let set = |target| GradeCommand::Set {
            target,
            source: inline.clone(),
            gesture: None,
        };
        assert_eq!(
            binding.copy_commands(&targets, &grading),
            vec![
                set(GradeTarget::Input(input(2))),
                set(layer),
                set(GradeTarget::Program)
            ]
        );

        // A linked library grade is copied by reference.
        grading.library.push(LibraryGradeStatus {
            grade: id(4),
            name: "Day".into(),
            value: Box::new(graded(-200)),
        });
        binding.apply_library(id(4), &graded(-200)).unwrap();
        assert_eq!(
            binding.copy_commands(&[GradeTarget::Program], &grading),
            vec![GradeCommand::Set {
                target: GradeTarget::Program,
                source: Some(WireGradeSource::Library(id(4))),
                gesture: None,
            }]
        );
        // A neutral draft clears the graded targets only.
        binding
            .document_mut()
            .set_param(GradeParam::Exposure, 0)
            .unwrap();
        assert_eq!(binding.copy_source(), None);
        assert_eq!(
            binding.copy_commands(
                &[GradeTarget::Input(input(2)), GradeTarget::Input(input(3))],
                &grading
            ),
            vec![GradeCommand::Set {
                target: GradeTarget::Input(input(3)),
                source: None,
                gesture: None,
            }]
        );
    }

    #[test]
    fn retargeting_starts_from_the_new_targets_grade_and_keeps_the_clipboard() {
        let mut grading = grading_with(GradeTarget::Input(input(1)), Some(graded(250)));
        grading.program = Some(WireGradeSource::Inline(Box::new(graded(-250))));
        let mut binding = GradeBinding::new(GradeTarget::Input(input(1)), &grading, 3);
        binding.document_mut().copy();
        binding
            .document_mut()
            .set_param(GradeParam::Exposure, 0)
            .unwrap();
        binding.retarget(GradeTarget::Program, &grading, 3);
        assert_eq!(binding.target(), GradeTarget::Program);
        assert_eq!(binding.document().grade(), &graded(-250));
        assert_eq!(binding.document().clipboard(), Some(&graded(250)));
        assert!(!binding.unsent());
    }

    #[test]
    fn library_ids_names_and_lut_limits() {
        let mut grading = GradingStatus::default();
        assert_eq!(next_library_id(&grading), Some(id(1)));
        for grade in [1, 2, 4] {
            grading.library.push(LibraryGradeStatus {
                grade: id(grade),
                name: format!("Look {grade}"),
                value: Box::new(Grade::IDENTITY),
            });
        }
        assert_eq!(next_library_id(&grading), Some(id(3)));
        assert_eq!(next_lut_id(&grading, None), Some(id(1)));
        assert_eq!(
            library_name_problem("Look 2", &grading),
            Some(GradeNameProblem::Taken)
        );
        assert_eq!(
            library_name_problem("  ", &grading),
            Some(GradeNameProblem::Blank)
        );
        assert_eq!(
            library_name_problem("a\tb", &grading),
            Some(GradeNameProblem::ControlCharacter)
        );
        assert_eq!(
            library_name_problem(&"x".repeat(129), &grading),
            Some(GradeNameProblem::TooLong)
        );
        assert_eq!(library_name_problem("Day", &grading), None);
        assert!(lut_fits_protocol(32 * 1024));
        assert!(!lut_fits_protocol(32 * 1024 + 1));
        assert_eq!(lut_uri(id(7)), "asset://luts/7.cube");
        assert_eq!(lut_id("asset://luts/7.cube"), Some(id(7)));
        assert_eq!(lut_id("asset://luts/07.cube"), None);
        assert_eq!(lut_id("asset://luts/film.cube"), None);
        let lut = GradeLut::new(lut_uri(id(7)), 500);
        assert_eq!(lut_id(&lut.asset_uri), Some(id(7)));
    }

    /// Regression: Studio imported under the smallest free LUT id, so a
    /// removed LUT's id came straight back, and the daemon (whose bundled
    /// LUT files are immutable) refused it. Ids go above the highest seen,
    /// past the last one this session issued, and wrap after `u32::MAX`.
    #[test]
    fn lut_ids_are_not_reused() {
        let mut grading = GradingStatus::default();
        for lut in [1, 3] {
            grading.luts.push(crate::GradeLutStatus {
                lut: id(lut),
                name: format!("LUT {lut}"),
                kind: fm_grade::CubeKind::ThreeD,
                size: 2,
            });
        }
        assert_eq!(next_lut_id(&grading, None), Some(id(4)));
        // LUT 4 was issued (and refused, or removed since): never again.
        assert_eq!(next_lut_id(&grading, Some(id(4))), Some(id(5)));
        assert_eq!(next_lut_id(&grading, Some(id(2))), Some(id(4)));
        grading.luts[1].lut = id(u32::MAX);
        assert_eq!(next_lut_id(&grading, None), Some(id(2)));
        assert_eq!(next_lut_id(&grading, Some(id(2))), Some(id(3)));
    }
}
