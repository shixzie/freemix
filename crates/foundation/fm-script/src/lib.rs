//! `FreeMix` Script (RC-011): a deliberately tiny, sandboxed automation
//! language.
//!
//! A script is plain text, one statement per line. It can hold integer,
//! boolean, and string variables, branch, loop, and run show actions; it
//! cannot read files, open sockets, start processes, read clocks, or reach
//! anything else in the host. The only way out of the sandbox is
//! [`ScriptHost`]: two read-only values (the Program and Preview input
//! numbers) and [`ScriptHost::act`], which the engine implements by running
//! the action as an ordinary engine command, so every refusal, padlock, and
//! replication rule of that command applies.
//!
//! Every run is deterministic (the same source, host answers, and limits
//! always produce the same actions and report) and bounded by [`Limits`]:
//! an instruction budget (every statement, loop turn, and expression node
//! costs one, so a run's time is linear in its budget), an action budget,
//! a memory budget (variable count and string length), and a wait budget
//! (how many `wait`s a run passes and how many frames they add up to). The
//! budgets cover the whole run, across its waits.
//!
//! A run that reaches `wait` stops with a [`Suspension`]: the frames to wait
//! and a [`Suspended`] state that is plain data (the statement path to the
//! `wait`, loop counters, variables, and budget use), bounded by the same
//! limits, so a host can save it and later [`resume`] the run right after
//! the `wait`, reading Program and Preview as they are then. The parser
//! bounds source size, line count, block nesting, and expression nesting,
//! so neither the parser nor the interpreter can recurse without limit.
//!
//! ```text
//! # Count down on the lower third, then take camera 2 with a fade.
//! let n = 3
//! while n > 0
//!   title_text "Lower third", 1, "Starting in " + n
//!   let n = n - 1
//! end
//! if program != 2
//!   transition fade, 25, 2
//! end
//! ```
//!
//! Statements:
//!
//! - `let NAME = EXPR` defines or reassigns a run-local variable.
//! - `if EXPR` … [`elif EXPR` …] [`else` …] `end`.
//! - `repeat EXPR` … `end` runs its body a fixed number of times.
//! - `while EXPR` … `end` runs while the condition holds.
//! - `stop` ends the run successfully.
//! - `wait EXPR` pauses the run for that many frames (1 or more): the run
//!   suspends and the host resumes it on a later frame (see [`resume`]).
//! - An action verb with comma-separated arguments (see [`Verb`]).
//!
//! Expressions use integers (64-bit, overflow is an error), `true` and
//! `false`, double-quoted strings (`\"` and `\\` escapes), variables, the
//! read-only `program` and `preview`, parentheses, `+ - * / %` (`+` with a
//! string concatenates), `== != < <= > >=`, and `and`, `or`, `not`.
//! An input argument is an input number (its 1-based position in the
//! project) or its exact name as a string. `#` starts a comment.

mod lexer;
mod machine;
mod parser;

use core::fmt;

pub use machine::{resume, run};
pub use parser::parse;

/// Longest script source, in UTF-8 bytes.
pub const MAX_SOURCE_BYTES: usize = 4_096;
/// Most lines one script holds.
pub const MAX_SOURCE_LINES: usize = 256;
/// Deepest nesting of `if`, `repeat`, and `while` blocks.
pub const MAX_BLOCK_DEPTH: usize = 16;
/// Deepest nesting of one expression (parentheses and operators).
pub const MAX_EXPRESSION_DEPTH: usize = 32;
/// Default instruction budget of one run.
pub const DEFAULT_INSTRUCTIONS: u32 = 10_000;
/// Default action budget of one run.
pub const DEFAULT_ACTIONS: u32 = 64;
/// Default variable budget of one run.
pub const DEFAULT_VARIABLES: u32 = 32;
/// Default longest string value, in UTF-8 bytes.
pub const DEFAULT_STRING_BYTES: u32 = 256;
/// Default count of `wait`s one run may pass.
pub const DEFAULT_WAITS: u32 = 32;
/// Default total frames one run's `wait`s may add up to: ten minutes at
/// 25 frames per second (a host sets the bound for its own rate).
pub const DEFAULT_WAIT_FRAMES: u32 = 15_000;

/// The budgets of one run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    /// Statements, loop turns, and expression nodes one run may execute.
    pub instructions: u32,
    /// Actions one run may perform.
    pub actions: u32,
    /// Distinct variables one run may define.
    pub variables: u32,
    /// Longest string value, in UTF-8 bytes.
    pub string_bytes: u32,
    /// `wait`s one run may pass; zero refuses every `wait`.
    pub waits: u32,
    /// Total frames one run's `wait`s may add up to.
    pub wait_frames: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            instructions: DEFAULT_INSTRUCTIONS,
            actions: DEFAULT_ACTIONS,
            variables: DEFAULT_VARIABLES,
            string_bytes: DEFAULT_STRING_BYTES,
            waits: DEFAULT_WAITS,
            wait_frames: DEFAULT_WAIT_FRAMES,
        }
    }
}

/// A position in the source: 1-based line and column (in characters).
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Span {
    pub line: u32,
    pub column: u32,
}

impl Span {
    #[must_use]
    pub const fn new(line: u32, column: u32) -> Self {
        Self { line, column }
    }
}

impl fmt::Display for Span {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.line, self.column)
    }
}

/// Which budget a run exhausted.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Budget {
    Instructions,
    Actions,
    Variables,
    StringBytes,
    /// More `wait`s than [`Limits::waits`].
    Waits,
    /// `wait`s adding up to more than [`Limits::wait_frames`].
    WaitFrames,
}

impl Budget {
    /// The stable token of the budget.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Instructions => "instructions",
            Self::Actions => "actions",
            Self::Variables => "variables",
            Self::StringBytes => "string_bytes",
            Self::Waits => "waits",
            Self::WaitFrames => "wait_frames",
        }
    }
}

/// Why a script failed.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FailureKind {
    /// The source does not parse.
    Syntax,
    /// An expression or argument could not be evaluated (unknown variable,
    /// wrong type, overflow, division by zero, argument out of range).
    Runtime,
    /// A budget ran out.
    Budget(Budget),
    /// The host refused an action.
    Refused,
}

/// A failed parse or run: what, where, and an English explanation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScriptFailure {
    pub kind: FailureKind,
    pub at: Span,
    pub message: String,
}

impl ScriptFailure {
    pub(crate) fn new(kind: FailureKind, at: Span, message: impl Into<String>) -> Self {
        Self {
            kind,
            at,
            message: message.into(),
        }
    }
}

impl fmt::Display for ScriptFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.kind {
            FailureKind::Syntax => "syntax error",
            FailureKind::Runtime => "runtime error",
            FailureKind::Budget(_) => "budget exceeded",
            FailureKind::Refused => "action refused",
        };
        write!(formatter, "line {}: {kind}: {}", self.at, self.message)
    }
}

impl std::error::Error for ScriptFailure {}

/// What one run (or one resumed part of a run) did. The counts cover the
/// whole run so far, across its waits.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScriptReport {
    /// Actions the host accepted, in order.
    pub actions: u32,
    /// Instructions the run used.
    pub instructions: u32,
    /// Why the run stopped early, if it did.
    pub failure: Option<ScriptFailure>,
    /// The `wait` the run stopped at, if it did (never with a failure).
    pub suspension: Option<Suspension>,
}

/// A run stopped at a `wait`: how long to wait and what to resume.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Suspension {
    /// Frames to wait, at least one.
    pub frames: u32,
    pub state: Suspended,
}

/// A variable's value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScriptValue {
    Int(i64),
    Bool(bool),
    Str(String),
}

/// One step of the path from the program's top level to a `wait`.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct ResumePoint {
    /// The statement's 0-based index in its block.
    pub statement: u32,
    /// For an `if`, the 0-based arm taken (the arm count for `else`); for a
    /// `repeat`, the turns left after the current one; otherwise zero.
    pub branch: u64,
}

/// The saved state of a run stopped at a `wait`: plain, bounded data (the
/// path has at most [`MAX_BLOCK_DEPTH`] + 1 points, the variables and their
/// strings stay within the run's [`Limits`]), so a host can persist it and
/// [`resume`] the run later. It is only meaningful for the source it was
/// taken from.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Suspended {
    /// From the top level to the `wait` itself, outermost first.
    pub path: Vec<ResumePoint>,
    /// Defined variables in definition order.
    pub variables: Vec<(String, ScriptValue)>,
    pub instructions: u32,
    pub actions: u32,
    /// `wait`s passed, including the one stopped at.
    pub waits: u32,
    /// Frames those `wait`s add up to.
    pub wait_frames: u32,
}

/// An input an action names: its 1-based position in the project or its
/// exact name.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum InputRef {
    Number(u32),
    Name(String),
}

impl fmt::Display for InputRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(number) => write!(formatter, "input {number}"),
            Self::Name(name) => write!(formatter, "input \"{name}\""),
        }
    }
}

/// A timed transition effect `transition` names.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Effect {
    Fade,
    AlphaFade,
    Wipe,
    VerticalWipe,
    Slide,
    VerticalSlide,
    Zoom,
    Fly,
    FlyRotate,
    Cube,
    CubeZoom,
    CrossZoom,
    Merge,
}

impl Effect {
    /// Every effect, in declaration order.
    pub const ALL: [Self; 13] = [
        Self::Fade,
        Self::AlphaFade,
        Self::Wipe,
        Self::VerticalWipe,
        Self::Slide,
        Self::VerticalSlide,
        Self::Zoom,
        Self::Fly,
        Self::FlyRotate,
        Self::Cube,
        Self::CubeZoom,
        Self::CrossZoom,
        Self::Merge,
    ];

    /// The word a script writes.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Fade => "fade",
            Self::AlphaFade => "alpha_fade",
            Self::Wipe => "wipe",
            Self::VerticalWipe => "vertical_wipe",
            Self::Slide => "slide",
            Self::VerticalSlide => "vertical_slide",
            Self::Zoom => "zoom",
            Self::Fly => "fly",
            Self::FlyRotate => "fly_rotate",
            Self::Cube => "cube",
            Self::CubeZoom => "cube_zoom",
            Self::CrossZoom => "cross_zoom",
            Self::Merge => "merge",
        }
    }

    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|effect| effect.word() == word)
    }
}

/// How a take reaches Program.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TakeEffect {
    Cut,
    Transition(Effect),
    /// A Stinger slot (`1..=8`).
    Stinger(u8),
}

/// A media transport operation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MediaOperation {
    Play,
    Pause,
    Seek(u64),
    Restart,
}

/// A paged-input (slideshow or document) step.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PageOperation {
    Next,
    Previous,
    First,
    Last,
    /// Zero-based page `n`, clamped to the last page.
    Goto(u32),
}

/// One show action a script asks the host to perform. Numbers are only
/// range-checked to their type here; the host checks them against the show.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ScriptAction {
    Preview(InputRef),
    /// Takes `input` (or the current Preview) to Program.
    Take {
        input: Option<InputRef>,
        effect: TakeEffect,
        frames: u32,
    },
    OverlayOn {
        channel: u8,
        input: InputRef,
    },
    OverlayOff {
        channel: u8,
    },
    TitleOn {
        input: InputRef,
        channel: u8,
    },
    TitleText {
        input: InputRef,
        field: u64,
        text: String,
    },
    /// Sets (`add` false) or adds `value` to (`add` true) one number field.
    TitleNumber {
        input: InputRef,
        field: u64,
        add: bool,
        value: i64,
    },
    TitleOff(InputRef),
    FadeToBlack {
        active: bool,
        frames: u32,
    },
    PlaylistStart,
    PlaylistStop,
    Gain {
        input: InputRef,
        millidb: i32,
    },
    Mute {
        input: InputRef,
        muted: bool,
    },
    Record {
        recorder: u8,
        start: bool,
    },
    Stream {
        target: u64,
        start: bool,
    },
    Media {
        input: InputRef,
        operation: MediaOperation,
    },
    /// Steps paged input `input` (a slideshow or document).
    Page {
        input: InputRef,
        operation: PageOperation,
    },
}

/// The action verbs, one per statement kind that acts on the show.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Verb {
    /// `preview INPUT`
    Preview,
    /// `cut [INPUT]`
    Cut,
    /// `transition EFFECT, FRAMES[, INPUT]`
    Transition,
    /// `stinger SLOT, FRAMES[, INPUT]`
    Stinger,
    /// `overlay CHANNEL, INPUT`
    Overlay,
    /// `overlay_off CHANNEL`
    OverlayOff,
    /// `title_on INPUT, CHANNEL`
    TitleOn,
    /// `title_text INPUT, FIELD, TEXT`
    TitleText,
    /// `title_number INPUT, FIELD, VALUE` sets a number field.
    TitleNumber,
    /// `title_add INPUT, FIELD, DELTA` adds to a number field.
    TitleAdd,
    /// `title_off INPUT`
    TitleOff,
    /// `ftb FRAMES` fades Program to black.
    Ftb,
    /// `ftb_off FRAMES` fades back from black.
    FtbOff,
    /// `playlist_start`
    PlaylistStart,
    /// `playlist_stop`
    PlaylistStop,
    /// `gain INPUT, MILLIDB`
    Gain,
    /// `mute INPUT`
    Mute,
    /// `unmute INPUT`
    Unmute,
    /// `record_start RECORDER`
    RecordStart,
    /// `record_stop RECORDER`
    RecordStop,
    /// `stream_start TARGET`
    StreamStart,
    /// `stream_stop TARGET`
    StreamStop,
    /// `play INPUT`
    Play,
    /// `pause INPUT`
    Pause,
    /// `seek INPUT, FRAME`
    Seek,
    /// `restart INPUT`
    Restart,
    /// `page_next INPUT`
    PageNext,
    /// `page_previous INPUT`
    PagePrevious,
    /// `page_first INPUT`
    PageFirst,
    /// `page_last INPUT`
    PageLast,
    /// `page_goto INPUT, PAGE`
    PageGoto,
}

impl Verb {
    /// Every verb, in declaration order.
    pub const ALL: [Self; 31] = [
        Self::Preview,
        Self::Cut,
        Self::Transition,
        Self::Stinger,
        Self::Overlay,
        Self::OverlayOff,
        Self::TitleOn,
        Self::TitleText,
        Self::TitleNumber,
        Self::TitleAdd,
        Self::TitleOff,
        Self::Ftb,
        Self::FtbOff,
        Self::PlaylistStart,
        Self::PlaylistStop,
        Self::Gain,
        Self::Mute,
        Self::Unmute,
        Self::RecordStart,
        Self::RecordStop,
        Self::StreamStart,
        Self::StreamStop,
        Self::Play,
        Self::Pause,
        Self::Seek,
        Self::Restart,
        Self::PageNext,
        Self::PagePrevious,
        Self::PageFirst,
        Self::PageLast,
        Self::PageGoto,
    ];

    /// The word a script writes.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Preview => "preview",
            Self::Cut => "cut",
            Self::Transition => "transition",
            Self::Stinger => "stinger",
            Self::Overlay => "overlay",
            Self::OverlayOff => "overlay_off",
            Self::TitleOn => "title_on",
            Self::TitleText => "title_text",
            Self::TitleNumber => "title_number",
            Self::TitleAdd => "title_add",
            Self::TitleOff => "title_off",
            Self::Ftb => "ftb",
            Self::FtbOff => "ftb_off",
            Self::PlaylistStart => "playlist_start",
            Self::PlaylistStop => "playlist_stop",
            Self::Gain => "gain",
            Self::Mute => "mute",
            Self::Unmute => "unmute",
            Self::RecordStart => "record_start",
            Self::RecordStop => "record_stop",
            Self::StreamStart => "stream_start",
            Self::StreamStop => "stream_stop",
            Self::Play => "play",
            Self::Pause => "pause",
            Self::Seek => "seek",
            Self::Restart => "restart",
            Self::PageNext => "page_next",
            Self::PagePrevious => "page_previous",
            Self::PageFirst => "page_first",
            Self::PageLast => "page_last",
            Self::PageGoto => "page_goto",
        }
    }

    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|verb| verb.word() == word)
    }

    /// The fewest and most expression arguments the verb takes (for
    /// `transition`, after its effect word).
    #[must_use]
    pub const fn arity(self) -> (usize, usize) {
        match self {
            Self::PlaylistStart | Self::PlaylistStop => (0, 0),
            Self::Cut => (0, 1),
            Self::Transition => (1, 2),
            Self::Stinger => (2, 3),
            Self::Overlay | Self::TitleOn | Self::Gain | Self::Seek | Self::PageGoto => (2, 2),
            Self::TitleText | Self::TitleNumber | Self::TitleAdd => (3, 3),
            Self::Preview
            | Self::OverlayOff
            | Self::TitleOff
            | Self::Ftb
            | Self::FtbOff
            | Self::Mute
            | Self::Unmute
            | Self::RecordStart
            | Self::RecordStop
            | Self::StreamStart
            | Self::StreamStop
            | Self::Play
            | Self::Pause
            | Self::Restart
            | Self::PageNext
            | Self::PagePrevious
            | Self::PageFirst
            | Self::PageLast => (1, 1),
        }
    }
}

/// Words a variable may not be named.
pub const KEYWORDS: [&str; 16] = [
    "let", "if", "elif", "else", "end", "repeat", "while", "stop", "wait", "and", "or", "not",
    "true", "false", "program", "preview",
];

/// The host a script acts through: the only capabilities a script has.
pub trait ScriptHost {
    /// The Program input's 1-based number, `0` when none.
    fn program(&self) -> u32;
    /// The Preview input's 1-based number, `0` when none.
    fn preview(&self) -> u32;
    /// Performs one action.
    ///
    /// # Errors
    ///
    /// Returns the English reason the host refused the action; the run
    /// stops there.
    fn act(&mut self, action: &ScriptAction) -> Result<(), String>;
}

/// A parsed script, ready to [`run`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Program {
    pub(crate) statements: Vec<parser::Statement>,
}

impl Program {
    /// Every action verb the program holds, in any branch or loop, in
    /// [`Verb::ALL`] order. Every action any run of the program can ask for
    /// is one of these verbs, whatever the host answers, so the set bounds
    /// what the program can do before it runs.
    #[must_use]
    pub fn verbs(&self) -> Vec<Verb> {
        let mut found = [false; Verb::ALL.len()];
        let mut blocks: Vec<&[parser::Statement]> = vec![&self.statements];
        while let Some(block) = blocks.pop() {
            for statement in block {
                match &statement.kind {
                    parser::StatementKind::Action { verb, .. } => {
                        if let Some(index) = Verb::ALL.iter().position(|known| known == verb) {
                            found[index] = true;
                        }
                    }
                    parser::StatementKind::If { arms, otherwise } => {
                        blocks.extend(arms.iter().map(|(_, _, body)| body.as_slice()));
                        blocks.push(otherwise);
                    }
                    parser::StatementKind::Repeat { body, .. }
                    | parser::StatementKind::While { body, .. } => blocks.push(body),
                    parser::StatementKind::Let { .. }
                    | parser::StatementKind::Stop
                    | parser::StatementKind::Wait { .. } => {}
                }
            }
        }
        Verb::ALL
            .into_iter()
            .zip(found)
            .filter_map(|(verb, used)| used.then_some(verb))
            .collect()
    }

    /// Whether the program holds a `wait`, in any branch or loop: whether a
    /// run of it may span frames.
    #[must_use]
    pub fn waits(&self) -> bool {
        let mut blocks: Vec<&[parser::Statement]> = vec![&self.statements];
        while let Some(block) = blocks.pop() {
            for statement in block {
                match &statement.kind {
                    parser::StatementKind::Wait { .. } => return true,
                    parser::StatementKind::If { arms, otherwise } => {
                        blocks.extend(arms.iter().map(|(_, _, body)| body.as_slice()));
                        blocks.push(otherwise);
                    }
                    parser::StatementKind::Repeat { body, .. }
                    | parser::StatementKind::While { body, .. } => blocks.push(body),
                    parser::StatementKind::Let { .. }
                    | parser::StatementKind::Stop
                    | parser::StatementKind::Action { .. } => {}
                }
            }
        }
        false
    }

    /// Where the `wait` a run in `suspended` stopped at is written (its
    /// 1-based line and column), or `None` when `suspended` does not lead
    /// through this program to a `wait`. A host reports it as the step a
    /// waiting run is at.
    #[must_use]
    pub fn wait_at(&self, suspended: &Suspended) -> Option<Span> {
        machine::wait_at(&self.statements, &suspended.path)
    }

    /// Whether `suspended` is a state a run of this program under `limits`
    /// can stop in: its path leads through this program's blocks to a
    /// `wait`, and its variables and budget use are within `limits`. A host
    /// restoring a saved state checks it with this before [`resume`].
    #[must_use]
    pub fn accepts(&self, suspended: &Suspended, limits: &Limits) -> bool {
        machine::path_is_valid(&self.statements, &suspended.path)
            && suspended.variables.len() <= limits.variables as usize
            && suspended
                .variables
                .iter()
                .enumerate()
                .all(|(index, (name, value))| {
                    lexer::is_identifier(name)
                        && !parser::is_reserved(name)
                        && suspended.variables[..index]
                            .iter()
                            .all(|(other, _)| other != name)
                        && match value {
                            ScriptValue::Str(text) => text.len() <= limits.string_bytes as usize,
                            ScriptValue::Int(_) | ScriptValue::Bool(_) => true,
                        }
                })
            && suspended.instructions <= limits.instructions
            && suspended.actions <= limits.actions
            && (1..=limits.waits).contains(&suspended.waits)
            && (1..=limits.wait_frames).contains(&suspended.wait_frames)
    }
}

/// Parses and runs `source`: a syntax error is reported like a run
/// failure, with no instructions used.
pub fn run_source(source: &str, host: &mut dyn ScriptHost, limits: Limits) -> ScriptReport {
    match parse(source) {
        Ok(program) => run(&program, host, limits),
        Err(failure) => ScriptReport {
            actions: 0,
            instructions: 0,
            failure: Some(failure),
            suspension: None,
        },
    }
}
