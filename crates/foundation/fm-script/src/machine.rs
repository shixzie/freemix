//! Runs parsed statements against a [`ScriptHost`] within [`Limits`].

use crate::parser::{BinaryOp, Builtin, Expr, ExprKind, Statement, StatementKind, UnaryOp};
use crate::{
    Budget, Effect, FailureKind, InputRef, Limits, MediaOperation, Program, ResumePoint,
    ScriptAction, ScriptFailure, ScriptHost, ScriptReport, Span, Suspended, Suspension, TakeEffect,
    Verb,
};

use crate::ScriptValue as Value;

impl Value {
    const fn type_name(&self) -> &'static str {
        match self {
            Self::Int(_) => "a number",
            Self::Bool(_) => "true or false",
            Self::Str(_) => "a string",
        }
    }
}

#[derive(Clone, Copy)]
enum Flow {
    Next,
    Stop,
    /// A `wait` of this many frames suspended the run; every block on the
    /// way out recorded where it stood in [`Machine::path`].
    Wait(u32),
}

struct Machine<'h> {
    host: &'h mut dyn ScriptHost,
    limits: Limits,
    variables: Vec<(String, Value)>,
    instructions: u32,
    actions: u32,
    waits: u32,
    wait_frames: u32,
    /// The path to a `wait` being unwound, innermost first.
    path: Vec<ResumePoint>,
    /// The branch of the statement being unwound (see
    /// [`ResumePoint::branch`]), set before it returns [`Flow::Wait`].
    branch: u64,
}

fn runtime(at: Span, message: impl Into<String>) -> ScriptFailure {
    ScriptFailure::new(FailureKind::Runtime, at, message)
}

fn mismatch() -> ScriptFailure {
    runtime(
        Span::new(1, 1),
        "the saved run does not match the script's source",
    )
}

/// Runs `program`: every statement in order until the end, a `stop`, a
/// `wait` (the report then holds the [`crate::Suspension`] to
/// [`resume`]), or the first failure. Actions the host accepted before a
/// failure stay accepted; a host that must be all-or-nothing discards them
/// itself.
pub fn run(program: &Program, host: &mut dyn ScriptHost, limits: Limits) -> ScriptReport {
    let mut machine = Machine::new(host, limits);
    let flow = machine.block(&program.statements, 0);
    machine.report(flow)
}

/// Resumes a run of `program` suspended at a `wait` in `suspended` (see
/// [`crate::Suspension`]): the run continues right after that `wait`, with
/// its variables and budget use, reading the host as it is now. A state
/// that does not fit `program` under `limits` fails with a runtime error at
/// line 1 (check it first with [`Program::accepts`]).
pub fn resume(
    program: &Program,
    suspended: &Suspended,
    host: &mut dyn ScriptHost,
    limits: Limits,
) -> ScriptReport {
    let mut machine = Machine::new(host, limits);
    if !program.accepts(suspended, &limits) {
        return machine.report(Err(mismatch()));
    }
    machine.variables.clone_from(&suspended.variables);
    machine.instructions = suspended.instructions;
    machine.actions = suspended.actions;
    machine.waits = suspended.waits;
    machine.wait_frames = suspended.wait_frames;
    let flow = machine.resume_block(&program.statements, &suspended.path);
    machine.report(flow)
}

/// Whether `path` leads through `statements` to a `wait`.
pub(crate) fn path_is_valid(statements: &[Statement], path: &[ResumePoint]) -> bool {
    let Some((point, rest)) = path.split_first() else {
        return false;
    };
    let Some(statement) = usize::try_from(point.statement)
        .ok()
        .and_then(|index| statements.get(index))
    else {
        return false;
    };
    match (&statement.kind, rest.is_empty()) {
        (StatementKind::Wait { .. }, true) => point.branch == 0,
        (StatementKind::If { arms, otherwise }, false) => {
            let Ok(branch) = usize::try_from(point.branch) else {
                return false;
            };
            match branch.cmp(&arms.len()) {
                core::cmp::Ordering::Less => path_is_valid(&arms[branch].2, rest),
                core::cmp::Ordering::Equal => path_is_valid(otherwise, rest),
                core::cmp::Ordering::Greater => false,
            }
        }
        (StatementKind::Repeat { body, .. }, false) => {
            i64::try_from(point.branch).is_ok() && path_is_valid(body, rest)
        }
        (StatementKind::While { body, .. }, false) => {
            point.branch == 0 && path_is_valid(body, rest)
        }
        _ => false,
    }
}

impl<'h> Machine<'h> {
    fn new(host: &'h mut dyn ScriptHost, limits: Limits) -> Self {
        Self {
            host,
            limits,
            variables: Vec::new(),
            instructions: 0,
            actions: 0,
            waits: 0,
            wait_frames: 0,
            path: Vec::new(),
            branch: 0,
        }
    }

    fn report(self, flow: Result<Flow, ScriptFailure>) -> ScriptReport {
        let (failure, suspension) = match flow {
            Ok(Flow::Next | Flow::Stop) => (None, None),
            Ok(Flow::Wait(frames)) => {
                let mut path = self.path;
                path.reverse();
                (
                    None,
                    Some(Suspension {
                        frames,
                        state: Suspended {
                            path,
                            variables: self.variables,
                            instructions: self.instructions,
                            actions: self.actions,
                            waits: self.waits,
                            wait_frames: self.wait_frames,
                        },
                    }),
                )
            }
            Err(failure) => (Some(failure), None),
        };
        ScriptReport {
            actions: self.actions,
            instructions: self.instructions,
            failure,
            suspension,
        }
    }
}

impl Machine<'_> {
    fn tick(&mut self, at: Span) -> Result<(), ScriptFailure> {
        if self.instructions >= self.limits.instructions {
            return Err(ScriptFailure::new(
                FailureKind::Budget(Budget::Instructions),
                at,
                format!(
                    "the script ran more than {} instructions",
                    self.limits.instructions
                ),
            ));
        }
        self.instructions += 1;
        Ok(())
    }

    /// Runs `statements` from index `from`.
    fn block(&mut self, statements: &[Statement], from: usize) -> Result<Flow, ScriptFailure> {
        for (index, statement) in statements.iter().enumerate().skip(from) {
            let flow = self.statement(statement)?;
            if let Some(flow) = self.unwind(flow, index) {
                return Ok(flow);
            }
        }
        Ok(Flow::Next)
    }

    /// Passes a `stop` or `wait` out of a block, recording where statement
    /// `index` stood for a `wait`; `None` to go on with the next statement.
    fn unwind(&mut self, flow: Flow, index: usize) -> Option<Flow> {
        match flow {
            Flow::Next => None,
            Flow::Stop => Some(Flow::Stop),
            Flow::Wait(frames) => {
                self.path.push(ResumePoint {
                    statement: u32::try_from(index).expect("blocks hold at most 256 statements"),
                    branch: self.branch,
                });
                Some(Flow::Wait(frames))
            }
        }
    }

    /// Resumes `statements` at `path` (see [`resume`]), then runs the rest
    /// of the block.
    fn resume_block(
        &mut self,
        statements: &[Statement],
        path: &[ResumePoint],
    ) -> Result<Flow, ScriptFailure> {
        let (point, rest) = path.split_first().ok_or_else(mismatch)?;
        let index = usize::try_from(point.statement).map_err(|_| mismatch())?;
        let statement = statements.get(index).ok_or_else(mismatch)?;
        let flow = if rest.is_empty() {
            // The `wait` itself: it has ended.
            Flow::Next
        } else {
            self.resume_statement(statement, point.branch, rest)?
        };
        if let Some(flow) = self.unwind(flow, index) {
            return Ok(flow);
        }
        self.block(statements, index + 1)
    }

    fn resume_statement(
        &mut self,
        statement: &Statement,
        branch: u64,
        rest: &[ResumePoint],
    ) -> Result<Flow, ScriptFailure> {
        match &statement.kind {
            StatementKind::If { arms, otherwise } => {
                let index = usize::try_from(branch).map_err(|_| mismatch())?;
                let body = match arms.get(index) {
                    Some((_, _, body)) => body,
                    None if index == arms.len() => otherwise,
                    None => return Err(mismatch()),
                };
                let flow = self.resume_block(body, rest)?;
                self.branch = branch;
                Ok(flow)
            }
            StatementKind::Repeat { body, .. } => match self.resume_block(body, rest)? {
                Flow::Next => self.repeat(statement.at, body, branch),
                Flow::Stop => Ok(Flow::Stop),
                Flow::Wait(frames) => {
                    self.branch = branch;
                    Ok(Flow::Wait(frames))
                }
            },
            StatementKind::While { condition, body } => match self.resume_block(body, rest)? {
                Flow::Next => self.repeat_while(statement.at, condition, body),
                Flow::Stop => Ok(Flow::Stop),
                Flow::Wait(frames) => {
                    self.branch = 0;
                    Ok(Flow::Wait(frames))
                }
            },
            _ => Err(mismatch()),
        }
    }

    /// Runs `body` `turns` more times.
    fn repeat(&mut self, at: Span, body: &[Statement], turns: u64) -> Result<Flow, ScriptFailure> {
        let mut left = turns;
        while left > 0 {
            left -= 1;
            self.tick(at)?;
            match self.block(body, 0)? {
                Flow::Next => {}
                Flow::Stop => return Ok(Flow::Stop),
                Flow::Wait(frames) => {
                    self.branch = left;
                    return Ok(Flow::Wait(frames));
                }
            }
        }
        Ok(Flow::Next)
    }

    /// Runs `body` while `condition` holds.
    fn repeat_while(
        &mut self,
        at: Span,
        condition: &Expr,
        body: &[Statement],
    ) -> Result<Flow, ScriptFailure> {
        while self.condition(condition)? {
            self.tick(at)?;
            match self.block(body, 0)? {
                Flow::Next => {}
                Flow::Stop => return Ok(Flow::Stop),
                Flow::Wait(frames) => {
                    self.branch = 0;
                    return Ok(Flow::Wait(frames));
                }
            }
        }
        Ok(Flow::Next)
    }

    fn statement(&mut self, statement: &Statement) -> Result<Flow, ScriptFailure> {
        self.tick(statement.at)?;
        match &statement.kind {
            StatementKind::Let { name, value } => {
                let value = self.eval(value)?;
                self.assign(name, value, statement.at)?;
            }
            StatementKind::If { arms, otherwise } => {
                for (index, (at, condition, body)) in arms.iter().enumerate() {
                    // Each `elif` tried costs one instruction, as a statement.
                    if index > 0 {
                        self.tick(*at)?;
                    }
                    if self.condition(condition)? {
                        let flow = self.block(body, 0)?;
                        self.branch = index as u64;
                        return Ok(flow);
                    }
                }
                let flow = self.block(otherwise, 0)?;
                self.branch = arms.len() as u64;
                return Ok(flow);
            }
            StatementKind::Repeat { count, body } => {
                let turns = match self.eval(count)? {
                    Value::Int(turns) if turns >= 0 => turns.unsigned_abs(),
                    Value::Int(turns) => {
                        return Err(runtime(
                            count.at,
                            format!("repeat needs a count of 0 or more, not {turns}"),
                        ));
                    }
                    other => {
                        return Err(runtime(
                            count.at,
                            format!("repeat needs a number, not {}", other.type_name()),
                        ));
                    }
                };
                return self.repeat(statement.at, body, turns);
            }
            StatementKind::While { condition, body } => {
                return self.repeat_while(statement.at, condition, body);
            }
            StatementKind::Stop => return Ok(Flow::Stop),
            StatementKind::Wait { frames } => return self.wait(frames, statement.at),
            StatementKind::Action {
                verb,
                effect,
                arguments,
            } => {
                let mut values = Vec::with_capacity(arguments.len());
                for argument in arguments {
                    values.push((self.eval(argument)?, argument.at));
                }
                let action = build_action(*verb, *effect, values, statement.at)?;
                if self.actions >= self.limits.actions {
                    return Err(ScriptFailure::new(
                        FailureKind::Budget(Budget::Actions),
                        statement.at,
                        format!("the script ran more than {} actions", self.limits.actions),
                    ));
                }
                self.host.act(&action).map_err(|message| {
                    ScriptFailure::new(FailureKind::Refused, statement.at, message)
                })?;
                self.actions += 1;
            }
        }
        Ok(Flow::Next)
    }

    fn wait(&mut self, frames: &Expr, at: Span) -> Result<Flow, ScriptFailure> {
        let frames = match self.eval(frames)? {
            Value::Int(value) => u32::try_from(value)
                .ok()
                .filter(|value| *value > 0)
                .ok_or_else(|| {
                    runtime(
                        frames.at,
                        format!("wait needs 1 or more frames, not {value}"),
                    )
                })?,
            other => {
                return Err(runtime(
                    frames.at,
                    format!("wait needs a number of frames, not {}", other.type_name()),
                ));
            }
        };
        if self.waits >= self.limits.waits {
            return Err(ScriptFailure::new(
                FailureKind::Budget(Budget::Waits),
                at,
                format!("the script waits more than {} times", self.limits.waits),
            ));
        }
        let total = self
            .wait_frames
            .checked_add(frames)
            .filter(|total| *total <= self.limits.wait_frames)
            .ok_or_else(|| {
                ScriptFailure::new(
                    FailureKind::Budget(Budget::WaitFrames),
                    at,
                    format!(
                        "the script waits more than {} frames in all",
                        self.limits.wait_frames
                    ),
                )
            })?;
        self.waits += 1;
        self.wait_frames = total;
        self.branch = 0;
        Ok(Flow::Wait(frames))
    }

    fn assign(&mut self, name: &str, value: Value, at: Span) -> Result<(), ScriptFailure> {
        if let Some(slot) = self
            .variables
            .iter_mut()
            .find(|(candidate, _)| candidate == name)
        {
            slot.1 = value;
            return Ok(());
        }
        if self.variables.len() >= self.limits.variables as usize {
            return Err(ScriptFailure::new(
                FailureKind::Budget(Budget::Variables),
                at,
                format!(
                    "the script defines more than {} variables",
                    self.limits.variables
                ),
            ));
        }
        self.variables.push((name.to_owned(), value));
        Ok(())
    }

    fn condition(&mut self, expression: &Expr) -> Result<bool, ScriptFailure> {
        match self.eval(expression)? {
            Value::Bool(value) => Ok(value),
            other => Err(runtime(
                expression.at,
                format!(
                    "a condition must be true or false, not {}",
                    other.type_name()
                ),
            )),
        }
    }

    fn string(&self, value: String, at: Span) -> Result<Value, ScriptFailure> {
        if value.len() > self.limits.string_bytes as usize {
            return Err(ScriptFailure::new(
                FailureKind::Budget(Budget::StringBytes),
                at,
                format!("a string is longer than {} bytes", self.limits.string_bytes),
            ));
        }
        Ok(Value::Str(value))
    }

    fn eval(&mut self, expression: &Expr) -> Result<Value, ScriptFailure> {
        self.tick(expression.at)?;
        let at = expression.at;
        Ok(match &expression.kind {
            ExprKind::Int(value) => Value::Int(*value),
            ExprKind::Bool(value) => Value::Bool(*value),
            ExprKind::Str(value) => self.string(value.clone(), at)?,
            ExprKind::Var(name) => self
                .variables
                .iter()
                .find(|(candidate, _)| candidate == name)
                .map(|(_, value)| value.clone())
                .ok_or_else(|| runtime(at, format!("{name} is not defined")))?,
            ExprKind::Builtin(Builtin::Program) => Value::Int(i64::from(self.host.program())),
            ExprKind::Builtin(Builtin::Preview) => Value::Int(i64::from(self.host.preview())),
            ExprKind::Unary(UnaryOp::Not, operand) => match self.eval(operand)? {
                Value::Bool(value) => Value::Bool(!value),
                other => {
                    return Err(runtime(
                        at,
                        format!("not needs true or false, not {}", other.type_name()),
                    ));
                }
            },
            ExprKind::Unary(UnaryOp::Neg, operand) => match self.eval(operand)? {
                Value::Int(value) => Value::Int(
                    value
                        .checked_neg()
                        .ok_or_else(|| runtime(at, "the number overflows"))?,
                ),
                other => {
                    return Err(runtime(
                        at,
                        format!("- needs a number, not {}", other.type_name()),
                    ));
                }
            },
            ExprKind::Binary(BinaryOp::And, left, right) => {
                Value::Bool(self.condition(left)? && self.condition(right)?)
            }
            ExprKind::Binary(BinaryOp::Or, left, right) => {
                Value::Bool(self.condition(left)? || self.condition(right)?)
            }
            ExprKind::Binary(op, left, right) => {
                let left = self.eval(left)?;
                let right = self.eval(right)?;
                self.binary(*op, left, right, at)?
            }
        })
    }

    fn binary(
        &self,
        op: BinaryOp,
        left: Value,
        right: Value,
        at: Span,
    ) -> Result<Value, ScriptFailure> {
        let overflow = || runtime(at, "the number overflows");
        Ok(match (op, left, right) {
            (BinaryOp::Add, Value::Int(left), Value::Int(right)) => {
                Value::Int(left.checked_add(right).ok_or_else(overflow)?)
            }
            (BinaryOp::Add, Value::Str(left), right) => {
                let right = match right {
                    Value::Str(text) => text,
                    Value::Int(value) => value.to_string(),
                    Value::Bool(value) => value.to_string(),
                };
                self.string(left + &right, at)?
            }
            (BinaryOp::Add, left, Value::Str(right)) => {
                let left = match left {
                    Value::Int(value) => value.to_string(),
                    Value::Bool(value) => value.to_string(),
                    Value::Str(text) => text,
                };
                self.string(left + &right, at)?
            }
            (BinaryOp::Sub, Value::Int(left), Value::Int(right)) => {
                Value::Int(left.checked_sub(right).ok_or_else(overflow)?)
            }
            (BinaryOp::Mul, Value::Int(left), Value::Int(right)) => {
                Value::Int(left.checked_mul(right).ok_or_else(overflow)?)
            }
            (BinaryOp::Div | BinaryOp::Rem, Value::Int(_), Value::Int(0)) => {
                return Err(runtime(at, "division by zero"));
            }
            (BinaryOp::Div, Value::Int(left), Value::Int(right)) => {
                Value::Int(left.checked_div(right).ok_or_else(overflow)?)
            }
            (BinaryOp::Rem, Value::Int(left), Value::Int(right)) => {
                Value::Int(left.checked_rem(right).ok_or_else(overflow)?)
            }
            (BinaryOp::Eq, left, right) => {
                Value::Bool(same_type(&left, &right, at)? && left == right)
            }
            (BinaryOp::Ne, left, right) => {
                Value::Bool(!(same_type(&left, &right, at)? && left == right))
            }
            (op @ (BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge), left, right) => {
                let ordering = match (&left, &right) {
                    (Value::Int(left), Value::Int(right)) => left.cmp(right),
                    (Value::Str(left), Value::Str(right)) => left.cmp(right),
                    _ => {
                        return Err(runtime(
                            at,
                            format!(
                                "cannot order {} and {}",
                                left.type_name(),
                                right.type_name()
                            ),
                        ));
                    }
                };
                Value::Bool(match op {
                    BinaryOp::Lt => ordering.is_lt(),
                    BinaryOp::Le => ordering.is_le(),
                    BinaryOp::Gt => ordering.is_gt(),
                    _ => ordering.is_ge(),
                })
            }
            (op, left, right) => {
                let symbol = match op {
                    BinaryOp::Add => "+",
                    BinaryOp::Sub => "-",
                    BinaryOp::Mul => "*",
                    BinaryOp::Div => "/",
                    _ => "%",
                };
                return Err(runtime(
                    at,
                    format!(
                        "{symbol} cannot combine {} and {}",
                        left.type_name(),
                        right.type_name()
                    ),
                ));
            }
        })
    }
}

/// `==` and `!=` compare values of one type only.
fn same_type(left: &Value, right: &Value, at: Span) -> Result<bool, ScriptFailure> {
    if core::mem::discriminant(left) == core::mem::discriminant(right) {
        Ok(true)
    } else {
        Err(runtime(
            at,
            format!(
                "cannot compare {} with {}",
                left.type_name(),
                right.type_name()
            ),
        ))
    }
}

/// Typed argument access for one action statement.
struct Arguments {
    verb: Verb,
    values: std::vec::IntoIter<(Value, Span)>,
    at: Span,
}

impl Arguments {
    fn next(&mut self) -> Option<(Value, Span)> {
        self.values.next()
    }

    fn integer<T: TryFrom<i64>>(&mut self, what: &str) -> Result<T, ScriptFailure> {
        let verb = self.verb.word();
        let (value, at) = self
            .next()
            .ok_or_else(|| runtime(self.at, format!("{verb} needs {what}")))?;
        match value {
            Value::Int(number) => T::try_from(number)
                .map_err(|_| runtime(at, format!("{number} is out of range for {what}"))),
            other => Err(runtime(
                at,
                format!("{verb} needs {what} as a number, not {}", other.type_name()),
            )),
        }
    }

    fn input(&mut self) -> Result<InputRef, ScriptFailure> {
        let verb = self.verb.word();
        let (value, at) = self
            .next()
            .ok_or_else(|| runtime(self.at, format!("{verb} needs an input")))?;
        Self::input_of(verb, value, at)
    }

    fn input_of(verb: &str, value: Value, at: Span) -> Result<InputRef, ScriptFailure> {
        match value {
            Value::Int(number) => u32::try_from(number)
                .ok()
                .filter(|number| *number > 0)
                .map(InputRef::Number)
                .ok_or_else(|| runtime(at, format!("there is no input number {number}"))),
            Value::Str(name) => Ok(InputRef::Name(name)),
            Value::Bool(_) => Err(runtime(
                at,
                format!("{verb} needs an input number or name, not true or false"),
            )),
        }
    }

    fn optional_input(&mut self) -> Result<Option<InputRef>, ScriptFailure> {
        let verb = self.verb.word();
        self.next()
            .map(|(value, at)| Self::input_of(verb, value, at))
            .transpose()
    }

    fn text(&mut self) -> Result<String, ScriptFailure> {
        let verb = self.verb.word();
        match self.next() {
            Some((Value::Str(text), _)) => Ok(text),
            Some((Value::Int(value), _)) => Ok(value.to_string()),
            Some((Value::Bool(_), at)) => {
                Err(runtime(at, format!("{verb} needs text, not true or false")))
            }
            None => Err(runtime(self.at, format!("{verb} needs text"))),
        }
    }
}

fn build_action(
    verb: Verb,
    effect: Option<Effect>,
    values: Vec<(Value, Span)>,
    at: Span,
) -> Result<ScriptAction, ScriptFailure> {
    let mut arguments = Arguments {
        verb,
        values: values.into_iter(),
        at,
    };
    let arguments = &mut arguments;
    Ok(match verb {
        Verb::Preview => ScriptAction::Preview(arguments.input()?),
        Verb::Cut => ScriptAction::Take {
            input: arguments.optional_input()?,
            effect: TakeEffect::Cut,
            frames: 0,
        },
        Verb::Transition => {
            let frames = arguments.integer("a duration in frames")?;
            ScriptAction::Take {
                input: arguments.optional_input()?,
                effect: TakeEffect::Transition(effect.expect("parsed transitions name an effect")),
                frames,
            }
        }
        Verb::Stinger => {
            let slot = arguments.integer("a Stinger slot")?;
            let frames = arguments.integer("a duration in frames")?;
            ScriptAction::Take {
                input: arguments.optional_input()?,
                effect: TakeEffect::Stinger(slot),
                frames,
            }
        }
        Verb::Overlay => {
            let channel = arguments.integer("an overlay channel")?;
            ScriptAction::OverlayOn {
                channel,
                input: arguments.input()?,
            }
        }
        Verb::OverlayOff => ScriptAction::OverlayOff {
            channel: arguments.integer("an overlay channel")?,
        },
        Verb::TitleOn => {
            let input = arguments.input()?;
            ScriptAction::TitleOn {
                input,
                channel: arguments.integer("an overlay channel")?,
            }
        }
        Verb::TitleText => {
            let input = arguments.input()?;
            let field = arguments.integer("a field number")?;
            ScriptAction::TitleText {
                input,
                field,
                text: arguments.text()?,
            }
        }
        Verb::TitleOff => ScriptAction::TitleOff(arguments.input()?),
        Verb::Ftb | Verb::FtbOff => ScriptAction::FadeToBlack {
            active: verb == Verb::Ftb,
            frames: arguments.integer("a duration in frames")?,
        },
        Verb::PlaylistStart => ScriptAction::PlaylistStart,
        Verb::PlaylistStop => ScriptAction::PlaylistStop,
        Verb::Gain => {
            let input = arguments.input()?;
            ScriptAction::Gain {
                input,
                millidb: arguments.integer("a gain in millidecibels")?,
            }
        }
        Verb::Mute | Verb::Unmute => ScriptAction::Mute {
            input: arguments.input()?,
            muted: verb == Verb::Mute,
        },
        Verb::RecordStart | Verb::RecordStop => ScriptAction::Record {
            recorder: arguments.integer("a recorder number")?,
            start: verb == Verb::RecordStart,
        },
        Verb::StreamStart | Verb::StreamStop => ScriptAction::Stream {
            target: arguments.integer("a stream target")?,
            start: verb == Verb::StreamStart,
        },
        Verb::Play | Verb::Pause | Verb::Restart => ScriptAction::Media {
            input: arguments.input()?,
            operation: match verb {
                Verb::Play => MediaOperation::Play,
                Verb::Pause => MediaOperation::Pause,
                _ => MediaOperation::Restart,
            },
        },
        Verb::Seek => {
            let input = arguments.input()?;
            ScriptAction::Media {
                input,
                operation: MediaOperation::Seek(arguments.integer("a frame")?),
            }
        }
    })
}
