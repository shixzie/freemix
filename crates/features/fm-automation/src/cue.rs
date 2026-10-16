//! Operator cue stacks (RC-017): ordered cues of caller-typed actions and
//! frame waits, a standby pointer, and deterministic GO, back, skip, jump,
//! stop, reset, and wait-advance planning.
//!
//! A [`CueStack`] never reads a clock and never performs an action: every
//! operation takes the frame it happens on and returns the actions due on
//! that frame, in order, with the next [`CueStackState`]. The caller runs
//! the returned actions together (frame-atomically) and installs the state
//! only when they all succeed.
//!
//! Firing a cue runs its steps from the first until a [`CueStep::Wait`]: the
//! remaining steps become [`CuePendingStep`] due that many frames later. A
//! cue that ends (all steps run) and has `auto_follow` fires the next cue in
//! list order on the same frame; a cue ending in a wait follows on once the
//! wait ends. Firing moves the standby pointer to the cue after the last cue
//! fired. A GO while another cue still waits drops that cue's remaining
//! steps: the newest GO owns the list.

use core::fmt;

/// One step of a cue: a caller action, or a wait of a whole number of
/// frames (at least one).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CueStep<A> {
    Action(A),
    Wait(u32),
}

/// One cue of a stack.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StackCue<A> {
    pub id: u64,
    pub steps: Vec<CueStep<A>>,
    pub auto_follow: bool,
}

/// A cue whose step `step` (and every later one) runs on `due_frame`.
/// `step` equals the cue's step count when only its auto-follow remains.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CuePendingStep {
    pub cue: u64,
    pub step: usize,
    pub due_frame: u64,
}

/// The run state of a stack.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CueStackState {
    /// The cue the next GO fires; `None` past the end of the list.
    pub standby: Option<u64>,
    /// The cue fired most recently.
    pub live: Option<u64>,
    pub pending: Option<CuePendingStep>,
    /// Every cue fired since creation or the last reset, ascending.
    pub played: Vec<u64>,
    /// The step that failed when a wait ended: `(cue, step)`.
    pub fault: Option<(u64, usize)>,
}

/// One action due on the firing frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FiredCueAction<A> {
    pub cue: u64,
    pub step: usize,
    pub action: A,
}

/// The outcome of a GO or wait advance: the actions due on the frame, in
/// order, the cues fired, and the state once they have run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CueFiring<A> {
    pub actions: Vec<FiredCueAction<A>>,
    pub fired: Vec<u64>,
    pub state: CueStackState,
}

/// Why a stack operation was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CueStackError {
    /// Two cues share an id.
    DuplicateCue(u64),
    /// A wait of zero frames.
    ZeroWait { cue: u64 },
    /// The stack holds no such cue.
    UnknownCue(u64),
    /// GO with nothing on standby (the list ran off its end or is empty).
    NothingOnStandby,
    /// No cue waits, or its wait ends after the advance frame.
    NothingDue { frame: u64 },
    /// A wait would end past the frame counter.
    FrameOverflow,
}

impl fmt::Display for CueStackError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateCue(cue) => write!(formatter, "cue {cue} is duplicated"),
            Self::ZeroWait { cue } => write!(formatter, "cue {cue} waits zero frames"),
            Self::UnknownCue(cue) => write!(formatter, "the cue list has no cue {cue}"),
            Self::NothingOnStandby => formatter.write_str("no cue is on standby"),
            Self::NothingDue { frame } => write!(formatter, "no cue is due at frame {frame}"),
            Self::FrameOverflow => formatter.write_str("the wait ends past the frame counter"),
        }
    }
}

impl std::error::Error for CueStackError {}

/// An ordered, validated cue stack.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CueStack<A> {
    cues: Vec<StackCue<A>>,
}

impl<A: Clone> CueStack<A> {
    /// A stack over `cues` in list order.
    ///
    /// # Errors
    ///
    /// Refuses duplicate cue ids and zero-frame waits.
    pub fn new(cues: Vec<StackCue<A>>) -> Result<Self, CueStackError> {
        for (index, cue) in cues.iter().enumerate() {
            if cues[..index].iter().any(|other| other.id == cue.id) {
                return Err(CueStackError::DuplicateCue(cue.id));
            }
            if cue
                .steps
                .iter()
                .any(|step| matches!(step, CueStep::Wait(0)))
            {
                return Err(CueStackError::ZeroWait { cue: cue.id });
            }
        }
        Ok(Self { cues })
    }

    #[must_use]
    pub fn cues(&self) -> &[StackCue<A>] {
        &self.cues
    }

    fn position(&self, cue: u64) -> Option<usize> {
        self.cues.iter().position(|candidate| candidate.id == cue)
    }

    fn next_after(&self, index: usize) -> Option<u64> {
        self.cues.get(index + 1).map(|cue| cue.id)
    }

    /// The state of a newly created list: the first cue on standby.
    #[must_use]
    pub fn initial_state(&self) -> CueStackState {
        CueStackState {
            standby: self.cues.first().map(|cue| cue.id),
            ..CueStackState::default()
        }
    }

    /// Fires `cue`, or the standby cue with `None`, on `frame`.
    ///
    /// # Errors
    ///
    /// Refuses an unknown cue, GO with nothing on standby, and a wait past
    /// the frame counter.
    pub fn go(
        &self,
        state: &CueStackState,
        cue: Option<u64>,
        frame: u64,
    ) -> Result<CueFiring<A>, CueStackError> {
        let cue = cue
            .or(state.standby)
            .ok_or(CueStackError::NothingOnStandby)?;
        let index = self.position(cue).ok_or(CueStackError::UnknownCue(cue))?;
        let mut next = state.clone();
        next.pending = None;
        next.fault = None;
        self.run(next, index, 0, frame)
    }

    /// Runs the waiting cue's remaining steps on `frame`, which must be its
    /// due frame or later.
    ///
    /// # Errors
    ///
    /// Refuses when nothing is due by `frame`, and a wait past the frame
    /// counter.
    pub fn advance(
        &self,
        state: &CueStackState,
        frame: u64,
    ) -> Result<CueFiring<A>, CueStackError> {
        let pending = state
            .pending
            .filter(|pending| pending.due_frame <= frame)
            .ok_or(CueStackError::NothingDue { frame })?;
        let index = self
            .position(pending.cue)
            .ok_or(CueStackError::UnknownCue(pending.cue))?;
        let mut next = state.clone();
        next.pending = None;
        self.run(next, index, pending.step, frame)
    }

    fn run(
        &self,
        mut state: CueStackState,
        mut index: usize,
        mut step: usize,
        frame: u64,
    ) -> Result<CueFiring<A>, CueStackError> {
        let mut actions = Vec::new();
        let mut fired = Vec::new();
        // Each pass fires one cue; auto-follow visits each cue at most once.
        for _ in 0..self.cues.len() {
            let cue = &self.cues[index];
            if step == 0 {
                fired.push(cue.id);
                state.live = Some(cue.id);
                if let Err(position) = state.played.binary_search(&cue.id) {
                    state.played.insert(position, cue.id);
                }
                state.standby = self.next_after(index);
            }
            while step < cue.steps.len() {
                match &cue.steps[step] {
                    CueStep::Action(action) => actions.push(FiredCueAction {
                        cue: cue.id,
                        step,
                        action: action.clone(),
                    }),
                    CueStep::Wait(frames) => {
                        let remaining = step + 1 < cue.steps.len()
                            || (cue.auto_follow && self.next_after(index).is_some());
                        if remaining {
                            state.pending = Some(CuePendingStep {
                                cue: cue.id,
                                step: step + 1,
                                due_frame: frame
                                    .checked_add(u64::from(*frames))
                                    .ok_or(CueStackError::FrameOverflow)?,
                            });
                            return Ok(CueFiring {
                                actions,
                                fired,
                                state,
                            });
                        }
                    }
                }
                step += 1;
            }
            match (cue.auto_follow, self.cues.get(index + 1)) {
                (true, Some(_)) => {
                    index += 1;
                    step = 0;
                }
                _ => break,
            }
        }
        Ok(CueFiring {
            actions,
            fired,
            state,
        })
    }

    /// Puts `cue` on standby (jump).
    ///
    /// # Errors
    ///
    /// Refuses an unknown cue.
    pub fn standby(&self, state: &CueStackState, cue: u64) -> Result<CueStackState, CueStackError> {
        self.position(cue).ok_or(CueStackError::UnknownCue(cue))?;
        Ok(CueStackState {
            standby: Some(cue),
            ..state.clone()
        })
    }

    /// Moves standby to the previous cue: from past the end to the last cue,
    /// and never before the first.
    #[must_use]
    pub fn back(&self, state: &CueStackState) -> CueStackState {
        let standby = match state.standby.and_then(|cue| self.position(cue)) {
            Some(index) => self.cues[index.saturating_sub(1)].id,
            None => match self.cues.last() {
                Some(cue) => cue.id,
                None => return state.clone(),
            },
        };
        CueStackState {
            standby: Some(standby),
            ..state.clone()
        }
    }

    /// Moves standby past its cue without firing it.
    #[must_use]
    pub fn skip(&self, state: &CueStackState) -> CueStackState {
        let standby = state
            .standby
            .and_then(|cue| self.position(cue))
            .and_then(|index| self.next_after(index));
        CueStackState {
            standby,
            ..state.clone()
        }
    }

    /// Drops the waiting cue's remaining steps.
    #[must_use]
    pub fn stop(&self, state: &CueStackState) -> CueStackState {
        CueStackState {
            pending: None,
            ..state.clone()
        }
    }

    /// Returns the list to its initial state.
    #[must_use]
    pub fn reset(&self) -> CueStackState {
        self.initial_state()
    }

    /// Records that step `step` of the waiting cue could not run: the cue
    /// stops there.
    #[must_use]
    pub fn fail(&self, state: &CueStackState, cue: u64, step: usize) -> CueStackState {
        CueStackState {
            pending: None,
            fault: Some((cue, step)),
            ..state.clone()
        }
    }

    /// The state carried across an edit that replaced `previous` (the old
    /// cue order) with this stack: a removed standby cue is replaced by the
    /// first surviving cue at or after its old position, references to
    /// removed cues are dropped, and a waiting cue keeps waiting while it
    /// still holds the step it resumes at (after a wait).
    #[must_use]
    pub fn reconcile(&self, previous: &[u64], state: &CueStackState) -> CueStackState {
        let holds = |cue: u64| self.position(cue).is_some();
        let standby = state.standby.and_then(|standby| {
            if holds(standby) {
                return Some(standby);
            }
            let old = previous.iter().position(|cue| *cue == standby)?;
            previous[old..].iter().copied().find(|cue| holds(*cue))
        });
        let pending = state.pending.filter(|pending| {
            self.position(pending.cue).is_some_and(|index| {
                let steps = &self.cues[index].steps;
                pending.step > 0
                    && pending.step <= steps.len()
                    && matches!(steps[pending.step - 1], CueStep::Wait(_))
            })
        });
        CueStackState {
            standby,
            live: state.live.filter(|cue| holds(*cue)),
            pending,
            played: state
                .played
                .iter()
                .copied()
                .filter(|cue| holds(*cue))
                .collect(),
            fault: state.fault.filter(|(cue, step)| {
                self.position(*cue)
                    .is_some_and(|index| *step < self.cues[index].steps.len())
            }),
        }
    }
}
