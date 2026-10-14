//! Validated macro scripts: ordered steps with waits, typed conditions,
//! run-local variables, nested calls, cancellation, and a bounded execution
//! log. Time comes only from caller-supplied timestamps.

use crate::{CommandIntent, Condition, ConditionContext, Value, condition::conditions_match};
use core::fmt;
use fm_command::{CommandEnvelope, Transaction};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Maximum steps in one macro, counting steps nested in branches.
pub const MAX_SCRIPT_STEPS: usize = 1024;
/// Maximum macros held by one [`ScriptEngine`].
pub const MAX_SCRIPT_MACROS: usize = 512;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeoutAction {
    /// Fail the run when the condition is still false at the deadline.
    Fail,
    /// Continue with the next step at the deadline.
    Continue,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ScriptStep<C> {
    /// Dispatch one command as a single-command transaction.
    Command(CommandIntent<C>),
    /// Dispatch an all-or-nothing transaction under one envelope.
    Atomic(Transaction<CommandIntent<C>>),
    /// Wait relative to the run's logical clock.
    Wait { ms: u64 },
    /// Wait until every condition holds on replicated state, bounded by a
    /// timeout relative to the run's logical clock.
    WaitUntil {
        conditions: Vec<Condition>,
        timeout_ms: u64,
        on_timeout: TimeoutAction,
    },
    If {
        conditions: Vec<Condition>,
        then: Vec<ScriptStep<C>>,
        otherwise: Vec<ScriptStep<C>>,
    },
    /// Run another installed macro inline.
    Call(String),
    /// Set a run-local variable visible to conditions as `var.<name>`.
    SetVariable { name: String, value: Value },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScriptMacro<C> {
    pub id: String,
    /// Checked against replicated state when a run starts.
    pub preconditions: Vec<Condition>,
    pub steps: Vec<ScriptStep<C>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScriptLimits {
    pub max_concurrent_runs: usize,
    /// Maximum nested frames (root, branches, and calls) in one run.
    pub max_depth: usize,
    /// Steps one run may execute per poll before yielding.
    pub max_steps_per_poll: usize,
    pub max_log_entries: usize,
}

impl Default for ScriptLimits {
    fn default() -> Self {
        Self {
            max_concurrent_runs: 16,
            max_depth: 8,
            max_steps_per_poll: 256,
            max_log_entries: 256,
        }
    }
}

/// Cancellation token for one run.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RunId(u64);

impl RunId {
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for RunId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScriptError {
    EmptyId,
    DuplicateId(String),
    EmptyMacro(String),
    TooManySteps {
        id: String,
        maximum: usize,
    },
    LibraryFull {
        maximum: usize,
    },
    UnknownMacro {
        referenced_by: String,
        missing: String,
    },
    Recursion {
        cycle: Vec<String>,
    },
    TooDeep {
        id: String,
        depth: usize,
        maximum: usize,
    },
    InUse {
        id: String,
        referenced_by: String,
    },
    NotInstalled(String),
    ConcurrencyLimit {
        maximum: usize,
    },
    PreconditionFailed(String),
    ClockRegression {
        last_ms: u64,
        now_ms: u64,
    },
    RunIdsExhausted,
}

impl fmt::Display for ScriptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyId => formatter.write_str("macro id must not be empty"),
            Self::DuplicateId(id) => write!(formatter, "macro {id} already exists"),
            Self::EmptyMacro(id) => write!(formatter, "macro {id} has no steps"),
            Self::TooManySteps { id, maximum } => {
                write!(formatter, "macro {id} exceeds {maximum} steps")
            }
            Self::LibraryFull { maximum } => {
                write!(formatter, "at most {maximum} macros may be installed")
            }
            Self::UnknownMacro {
                referenced_by,
                missing,
            } => write!(formatter, "macro {referenced_by} calls unknown {missing}"),
            Self::Recursion { cycle } => {
                write!(formatter, "macro call cycle {}", cycle.join(" -> "))
            }
            Self::TooDeep { id, depth, maximum } => {
                write!(
                    formatter,
                    "macro {id} nests {depth} frames, limit {maximum}"
                )
            }
            Self::InUse { id, referenced_by } => {
                write!(formatter, "macro {id} is called by {referenced_by}")
            }
            Self::NotInstalled(id) => write!(formatter, "macro {id} is not installed"),
            Self::ConcurrencyLimit { maximum } => {
                write!(formatter, "at most {maximum} macro runs may be active")
            }
            Self::PreconditionFailed(id) => write!(formatter, "macro {id} preconditions failed"),
            Self::ClockRegression { last_ms, now_ms } => {
                write!(formatter, "macro clock {now_ms}ms precedes {last_ms}ms")
            }
            Self::RunIdsExhausted => formatter.write_str("macro run ids exhausted"),
        }
    }
}

impl std::error::Error for ScriptError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScriptFailure {
    WaitTimedOut,
    TimestampOverflow,
    DepthExceeded,
    MacroMissing(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RunOutcome {
    Completed,
    Failed(ScriptFailure),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScriptLogEvent {
    Started,
    Dispatched { sequence: u32 },
    Waiting { until_ms: u64 },
    WaitingFor { deadline_ms: u64 },
    ConditionMet,
    TimeoutContinued,
    Branch { took_then: bool },
    Called { macro_id: String },
    VariableSet { name: String },
    Completed,
    Failed(ScriptFailure),
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScriptLogEntry {
    pub at_ms: u64,
    pub run: RunId,
    pub macro_id: String,
    pub event: ScriptLogEvent,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScriptDispatch<C> {
    pub run: RunId,
    pub macro_id: String,
    /// One-based dispatch sequence within the run.
    pub sequence: u32,
    /// The run's logical time for this dispatch (a wait's deadline, not the
    /// poll time, so late polls do not accumulate drift).
    pub logical_ms: u64,
    pub envelope: CommandEnvelope<Transaction<CommandIntent<C>>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScriptPoll<C> {
    pub dispatches: Vec<ScriptDispatch<C>>,
    pub finished: Vec<(RunId, RunOutcome)>,
}

impl<C> Default for ScriptPoll<C> {
    fn default() -> Self {
        Self {
            dispatches: Vec::new(),
            finished: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RunState {
    Ready,
    WaitingUntil { until_ms: u64 },
    WaitingFor { deadline_ms: u64 },
}

#[derive(Clone, Debug)]
struct Frame<C> {
    steps: Vec<ScriptStep<C>>,
    pc: usize,
}

#[derive(Clone, Debug)]
enum Block {
    Until(u64),
    Condition {
        conditions: Vec<Condition>,
        deadline_ms: u64,
        on_timeout: TimeoutAction,
    },
}

#[derive(Clone, Debug)]
struct Run<C> {
    macro_id: String,
    frames: Vec<Frame<C>>,
    clock_ms: u64,
    block: Option<Block>,
    variables: BTreeMap<String, Value>,
    dispatched: u32,
}

#[derive(Clone, Debug)]
pub struct ScriptEngine<C> {
    limits: ScriptLimits,
    library: BTreeMap<String, ScriptMacro<C>>,
    runs: BTreeMap<RunId, Run<C>>,
    next_run: u64,
    last_ms: Option<u64>,
    log: VecDeque<ScriptLogEntry>,
    dropped_log_entries: u64,
}

impl<C> Default for ScriptEngine<C> {
    fn default() -> Self {
        Self::new(ScriptLimits::default())
    }
}

impl<C> ScriptEngine<C> {
    #[must_use]
    pub fn new(limits: ScriptLimits) -> Self {
        Self {
            limits,
            library: BTreeMap::new(),
            runs: BTreeMap::new(),
            next_run: 1,
            last_ms: None,
            log: VecDeque::new(),
            dropped_log_entries: 0,
        }
    }
}

impl<C: Clone> ScriptEngine<C> {
    /// Installs macros as one all-or-nothing transaction. Calls may reference
    /// installed macros or macros in the same batch; unknown references, call
    /// cycles, and nesting beyond [`ScriptLimits::max_depth`] reject the whole
    /// batch and leave the library unchanged.
    ///
    /// # Errors
    ///
    /// Returns the first validation failure in batch order.
    pub fn install(
        &mut self,
        macros: impl IntoIterator<Item = ScriptMacro<C>>,
    ) -> Result<(), ScriptError> {
        let batch: Vec<_> = macros.into_iter().collect();
        let mut ids = BTreeSet::new();
        for script in &batch {
            if script.id.is_empty() {
                return Err(ScriptError::EmptyId);
            }
            if self.library.contains_key(&script.id) || !ids.insert(script.id.as_str()) {
                return Err(ScriptError::DuplicateId(script.id.clone()));
            }
            if script.steps.is_empty() {
                return Err(ScriptError::EmptyMacro(script.id.clone()));
            }
            if count_steps(&script.steps) > MAX_SCRIPT_STEPS {
                return Err(ScriptError::TooManySteps {
                    id: script.id.clone(),
                    maximum: MAX_SCRIPT_STEPS,
                });
            }
        }
        if self.library.len() + batch.len() > MAX_SCRIPT_MACROS {
            return Err(ScriptError::LibraryFull {
                maximum: MAX_SCRIPT_MACROS,
            });
        }
        let lookup = |id: &str| {
            batch
                .iter()
                .find(|script| script.id == id)
                .or_else(|| self.library.get(id))
        };
        let mut depths = BTreeMap::new();
        for script in &batch {
            let mut stack = Vec::new();
            let depth = macro_depth(script, &lookup, &mut stack, &mut depths)?;
            if depth > self.limits.max_depth {
                return Err(ScriptError::TooDeep {
                    id: script.id.clone(),
                    depth,
                    maximum: self.limits.max_depth,
                });
            }
        }
        for script in batch {
            self.library.insert(script.id.clone(), script);
        }
        Ok(())
    }

    /// Removes an installed macro that no other macro calls. Active runs keep
    /// executing their already-loaded steps.
    ///
    /// # Errors
    ///
    /// Returns an error if the macro is unknown or still referenced.
    pub fn uninstall(&mut self, id: &str) -> Result<ScriptMacro<C>, ScriptError> {
        if !self.library.contains_key(id) {
            return Err(ScriptError::NotInstalled(id.to_owned()));
        }
        if let Some(caller) = self
            .library
            .values()
            .find(|script| script.id != id && calls(&script.steps).contains(id))
        {
            return Err(ScriptError::InUse {
                id: id.to_owned(),
                referenced_by: caller.id.clone(),
            });
        }
        self.library
            .remove(id)
            .ok_or_else(|| ScriptError::NotInstalled(id.to_owned()))
    }

    #[must_use]
    pub fn is_installed(&self, id: &str) -> bool {
        self.library.contains_key(id)
    }

    /// Starts a run after checking preconditions against replicated state.
    /// The run first executes on the next [`Self::poll`].
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown macro, a regressing clock, the
    /// concurrency bound, or failing preconditions; nothing is started.
    pub fn start(
        &mut self,
        macro_id: &str,
        now_ms: u64,
        state: &ConditionContext,
    ) -> Result<RunId, ScriptError> {
        self.check_clock(now_ms)?;
        let script = self
            .library
            .get(macro_id)
            .ok_or_else(|| ScriptError::NotInstalled(macro_id.to_owned()))?;
        if self.runs.len() >= self.limits.max_concurrent_runs {
            return Err(ScriptError::ConcurrencyLimit {
                maximum: self.limits.max_concurrent_runs,
            });
        }
        if !conditions_match(&script.preconditions, state) {
            return Err(ScriptError::PreconditionFailed(macro_id.to_owned()));
        }
        let id = RunId(self.next_run);
        self.next_run = self
            .next_run
            .checked_add(1)
            .ok_or(ScriptError::RunIdsExhausted)?;
        let run = Run {
            macro_id: macro_id.to_owned(),
            frames: vec![Frame {
                steps: script.steps.clone(),
                pc: 0,
            }],
            clock_ms: now_ms,
            block: None,
            variables: BTreeMap::new(),
            dispatched: 0,
        };
        self.runs.insert(id, run);
        self.last_ms = Some(now_ms);
        self.record(now_ms, id, macro_id.to_owned(), ScriptLogEvent::Started);
        Ok(id)
    }

    /// Cancels a run before its next step; already-dispatched commands are
    /// not recalled. Returns `false` when the run is not active.
    pub fn cancel(&mut self, run: RunId, now_ms: u64) -> bool {
        let Some(state) = self.runs.remove(&run) else {
            return false;
        };
        self.record(now_ms, run, state.macro_id, ScriptLogEvent::Cancelled);
        true
    }

    /// Cancels every active run whose root macro is `macro_id`.
    pub fn cancel_macro(&mut self, macro_id: &str, now_ms: u64) -> Vec<RunId> {
        let ids: Vec<_> = self
            .runs
            .iter()
            .filter(|(_, run)| run.macro_id == macro_id)
            .map(|(id, _)| *id)
            .collect();
        for id in &ids {
            self.cancel(*id, now_ms);
        }
        ids
    }

    /// Cancels all active runs.
    pub fn cancel_all(&mut self, now_ms: u64) -> Vec<RunId> {
        let ids: Vec<_> = self.runs.keys().copied().collect();
        for id in &ids {
            self.cancel(*id, now_ms);
        }
        ids
    }

    #[must_use]
    pub fn run_state(&self, run: RunId) -> Option<RunState> {
        self.runs.get(&run).map(|run| match &run.block {
            None => RunState::Ready,
            Some(Block::Until(until_ms)) => RunState::WaitingUntil {
                until_ms: *until_ms,
            },
            Some(Block::Condition { deadline_ms, .. }) => RunState::WaitingFor {
                deadline_ms: *deadline_ms,
            },
        })
    }

    #[must_use]
    pub fn active_runs(&self) -> usize {
        self.runs.len()
    }

    pub fn log(&self) -> impl Iterator<Item = &ScriptLogEntry> {
        self.log.iter()
    }

    pub fn drain_log(&mut self) -> Vec<ScriptLogEntry> {
        self.log.drain(..).collect()
    }

    #[must_use]
    pub const fn dropped_log_entries(&self) -> u64 {
        self.dropped_log_entries
    }

    /// Advances every active run in run-id order until it blocks, finishes,
    /// or exhausts its per-poll step budget.
    ///
    /// # Errors
    ///
    /// Returns an error when `now_ms` precedes an earlier start or poll.
    pub fn poll(
        &mut self,
        now_ms: u64,
        state: &ConditionContext,
    ) -> Result<ScriptPoll<C>, ScriptError> {
        self.check_clock(now_ms)?;
        self.last_ms = Some(now_ms);
        let mut output = ScriptPoll::default();
        let ids: Vec<_> = self.runs.keys().copied().collect();
        for id in ids {
            let Some(mut run) = self.runs.remove(&id) else {
                continue;
            };
            let mut events = Vec::new();
            let outcome = self.advance(id, &mut run, now_ms, state, &mut output, &mut events);
            for (at_ms, event) in events {
                self.record(at_ms, id, run.macro_id.clone(), event);
            }
            match outcome {
                Some(outcome) => {
                    let event = match &outcome {
                        RunOutcome::Completed => ScriptLogEvent::Completed,
                        RunOutcome::Failed(failure) => ScriptLogEvent::Failed(failure.clone()),
                    };
                    self.record(run.clock_ms, id, run.macro_id.clone(), event);
                    output.finished.push((id, outcome));
                }
                None => {
                    self.runs.insert(id, run);
                }
            }
        }
        Ok(output)
    }

    fn advance(
        &self,
        id: RunId,
        run: &mut Run<C>,
        now_ms: u64,
        state: &ConditionContext,
        output: &mut ScriptPoll<C>,
        events: &mut Vec<(u64, ScriptLogEvent)>,
    ) -> Option<RunOutcome> {
        let mut budget = self.limits.max_steps_per_poll;
        loop {
            match run.block.take() {
                Some(Block::Until(until_ms)) if now_ms < until_ms => {
                    run.block = Some(Block::Until(until_ms));
                    return None;
                }
                Some(Block::Until(until_ms)) => run.clock_ms = until_ms,
                Some(Block::Condition {
                    conditions,
                    deadline_ms,
                    on_timeout,
                }) => {
                    if conditions_match(&conditions, &run.context(state)) {
                        run.clock_ms = run.clock_ms.max(now_ms.min(deadline_ms));
                        events.push((run.clock_ms, ScriptLogEvent::ConditionMet));
                    } else if now_ms >= deadline_ms {
                        run.clock_ms = deadline_ms;
                        if on_timeout == TimeoutAction::Fail {
                            return Some(RunOutcome::Failed(ScriptFailure::WaitTimedOut));
                        }
                        events.push((deadline_ms, ScriptLogEvent::TimeoutContinued));
                    } else {
                        run.block = Some(Block::Condition {
                            conditions,
                            deadline_ms,
                            on_timeout,
                        });
                        return None;
                    }
                }
                None => {}
            }
            if budget == 0 {
                return None;
            }
            let Some(frame) = run.frames.last_mut() else {
                return Some(RunOutcome::Completed);
            };
            let Some(step) = frame.steps.get(frame.pc).cloned() else {
                run.frames.pop();
                continue;
            };
            frame.pc += 1;
            budget -= 1;
            if let Some(outcome) = self.execute(id, run, step, state, output, events) {
                return Some(outcome);
            }
        }
    }

    fn execute(
        &self,
        id: RunId,
        run: &mut Run<C>,
        step: ScriptStep<C>,
        state: &ConditionContext,
        output: &mut ScriptPoll<C>,
        events: &mut Vec<(u64, ScriptLogEvent)>,
    ) -> Option<RunOutcome> {
        match step {
            ScriptStep::Command(intent) => {
                let Ok(transaction) = Transaction::new([intent]) else {
                    unreachable!("one command is a valid transaction");
                };
                output
                    .dispatches
                    .push(run.dispatch(id, transaction, events));
            }
            ScriptStep::Atomic(transaction) => {
                output
                    .dispatches
                    .push(run.dispatch(id, transaction, events));
            }
            ScriptStep::Wait { ms } => {
                let Some(until_ms) = run.clock_ms.checked_add(ms) else {
                    return Some(RunOutcome::Failed(ScriptFailure::TimestampOverflow));
                };
                events.push((run.clock_ms, ScriptLogEvent::Waiting { until_ms }));
                run.block = Some(Block::Until(until_ms));
            }
            ScriptStep::WaitUntil {
                conditions,
                timeout_ms,
                on_timeout,
            } => {
                let Some(deadline_ms) = run.clock_ms.checked_add(timeout_ms) else {
                    return Some(RunOutcome::Failed(ScriptFailure::TimestampOverflow));
                };
                events.push((run.clock_ms, ScriptLogEvent::WaitingFor { deadline_ms }));
                run.block = Some(Block::Condition {
                    conditions,
                    deadline_ms,
                    on_timeout,
                });
            }
            ScriptStep::If {
                conditions,
                then,
                otherwise,
            } => {
                let took_then = conditions_match(&conditions, &run.context(state));
                events.push((run.clock_ms, ScriptLogEvent::Branch { took_then }));
                let steps = if took_then { then } else { otherwise };
                if !steps.is_empty() && !run.push(steps, self.limits.max_depth) {
                    return Some(RunOutcome::Failed(ScriptFailure::DepthExceeded));
                }
            }
            ScriptStep::Call(macro_id) => {
                // The root may have been uninstalled after this run
                // started, which unpins its callees.
                let Some(callee) = self.library.get(&macro_id) else {
                    return Some(RunOutcome::Failed(ScriptFailure::MacroMissing(macro_id)));
                };
                events.push((run.clock_ms, ScriptLogEvent::Called { macro_id }));
                if !run.push(callee.steps.clone(), self.limits.max_depth) {
                    return Some(RunOutcome::Failed(ScriptFailure::DepthExceeded));
                }
            }
            ScriptStep::SetVariable { name, value } => {
                events.push((
                    run.clock_ms,
                    ScriptLogEvent::VariableSet { name: name.clone() },
                ));
                run.variables.insert(name, value);
            }
        }
        None
    }

    fn check_clock(&self, now_ms: u64) -> Result<(), ScriptError> {
        match self.last_ms {
            Some(last_ms) if now_ms < last_ms => {
                Err(ScriptError::ClockRegression { last_ms, now_ms })
            }
            _ => Ok(()),
        }
    }

    fn record(&mut self, at_ms: u64, run: RunId, macro_id: String, event: ScriptLogEvent) {
        if self.limits.max_log_entries == 0 {
            self.dropped_log_entries = self.dropped_log_entries.saturating_add(1);
            return;
        }
        if self.log.len() == self.limits.max_log_entries {
            self.log.pop_front();
            self.dropped_log_entries = self.dropped_log_entries.saturating_add(1);
        }
        self.log.push_back(ScriptLogEntry {
            at_ms,
            run,
            macro_id,
            event,
        });
    }
}

impl<C> Run<C> {
    fn context(&self, state: &ConditionContext) -> ConditionContext {
        let mut context = state.clone();
        for (name, value) in &self.variables {
            context.insert(format!("var.{name}"), value.clone());
        }
        context
    }

    fn push(&mut self, steps: Vec<ScriptStep<C>>, max_depth: usize) -> bool {
        if self.frames.len() >= max_depth {
            return false;
        }
        self.frames.push(Frame { steps, pc: 0 });
        true
    }

    fn dispatch(
        &mut self,
        id: RunId,
        transaction: Transaction<CommandIntent<C>>,
        events: &mut Vec<(u64, ScriptLogEvent)>,
    ) -> ScriptDispatch<C> {
        self.dispatched = self.dispatched.saturating_add(1);
        let sequence = self.dispatched;
        events.push((self.clock_ms, ScriptLogEvent::Dispatched { sequence }));
        let command_id = format!("{}:{}:{}", self.macro_id, id, sequence);
        ScriptDispatch {
            run: id,
            macro_id: self.macro_id.clone(),
            sequence,
            logical_ms: self.clock_ms,
            envelope: CommandEnvelope::new(command_id.clone(), command_id, transaction),
        }
    }
}

fn count_steps<C>(steps: &[ScriptStep<C>]) -> usize {
    steps
        .iter()
        .map(|step| match step {
            ScriptStep::If {
                then, otherwise, ..
            } => 1 + count_steps(then) + count_steps(otherwise),
            _ => 1,
        })
        .sum()
}

fn calls<C>(steps: &[ScriptStep<C>]) -> BTreeSet<&str> {
    let mut out = BTreeSet::new();
    for step in steps {
        match step {
            ScriptStep::Call(id) => {
                out.insert(id.as_str());
            }
            ScriptStep::If {
                then, otherwise, ..
            } => {
                out.extend(calls(then));
                out.extend(calls(otherwise));
            }
            _ => {}
        }
    }
    out
}

/// Frames needed to run `steps`: one for the list itself plus the deepest
/// nested branch or call.
fn steps_depth<'a, C: 'a>(
    owner: &str,
    steps: &'a [ScriptStep<C>],
    lookup: &impl Fn(&str) -> Option<&'a ScriptMacro<C>>,
    stack: &mut Vec<String>,
    memo: &mut BTreeMap<String, usize>,
) -> Result<usize, ScriptError> {
    let mut deepest = 0;
    for step in steps {
        let nested = match step {
            ScriptStep::If {
                then, otherwise, ..
            } => steps_depth(owner, then, lookup, stack, memo)?
                .max(steps_depth(owner, otherwise, lookup, stack, memo)?),
            ScriptStep::Call(callee) => {
                let script = lookup(callee).ok_or_else(|| ScriptError::UnknownMacro {
                    referenced_by: owner.to_owned(),
                    missing: callee.clone(),
                })?;
                macro_depth(script, lookup, stack, memo)?
            }
            _ => 0,
        };
        deepest = deepest.max(nested);
    }
    Ok(1 + deepest)
}

fn macro_depth<'a, C: 'a>(
    script: &'a ScriptMacro<C>,
    lookup: &impl Fn(&str) -> Option<&'a ScriptMacro<C>>,
    stack: &mut Vec<String>,
    memo: &mut BTreeMap<String, usize>,
) -> Result<usize, ScriptError> {
    if let Some(depth) = memo.get(&script.id) {
        return Ok(*depth);
    }
    if let Some(start) = stack.iter().position(|id| *id == script.id) {
        let mut cycle = stack[start..].to_vec();
        cycle.push(script.id.clone());
        return Err(ScriptError::Recursion { cycle });
    }
    stack.push(script.id.clone());
    let depth = steps_depth(&script.id, &script.steps, lookup, stack, memo)?;
    stack.pop();
    memo.insert(script.id.clone(), depth);
    Ok(depth)
}
