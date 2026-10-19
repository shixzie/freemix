//! `FreeMix` Script (RC-011): parsing, deterministic runs, budgets, and
//! refusals.

use fm_script::{
    Budget, Effect, FailureKind, InputRef, Limits, MAX_SOURCE_BYTES, MediaOperation, ScriptAction,
    ScriptHost, Span, TakeEffect, Verb, parse, run, run_source,
};

/// A host that records actions and refuses inputs above `inputs`.
#[derive(Default)]
struct Recorder {
    program: u32,
    preview: u32,
    inputs: u32,
    actions: Vec<ScriptAction>,
}

impl ScriptHost for Recorder {
    fn program(&self) -> u32 {
        self.program
    }

    fn preview(&self) -> u32 {
        self.preview
    }

    fn act(&mut self, action: &ScriptAction) -> Result<(), String> {
        let input = match action {
            ScriptAction::Preview(input) => Some(input),
            ScriptAction::Take { input, .. } => input.as_ref(),
            _ => None,
        };
        if let Some(InputRef::Number(number)) = input
            && *number > self.inputs
        {
            return Err(format!("the show has no input {number}"));
        }
        if let ScriptAction::Take {
            input: Some(InputRef::Number(number)),
            ..
        } = action
        {
            self.program = *number;
        }
        if let ScriptAction::Preview(InputRef::Number(number)) = action {
            self.preview = *number;
        }
        self.actions.push(action.clone());
        Ok(())
    }
}

fn host() -> Recorder {
    Recorder {
        program: 1,
        preview: 2,
        inputs: 4,
        actions: Vec::new(),
    }
}

#[test]
#[allow(clippy::too_many_lines)] // one case per verb
fn every_verb_builds_its_typed_action() {
    let source = r#"
# one of each
preview 2
cut
cut "Camera 3"
transition wipe, 25, 4
transition fade, 30
stinger 2, 40, 1
overlay 1, 3
overlay_off 1
title_on "Lower third", 2
title_text 4, 1, "Score: " + (2 + 1)
title_number 4, 2, 10 - 3
title_add "Lower third", 2, -1
title_off 4
ftb 25
ftb_off 25
playlist_start
playlist_stop
gain 3, -6000
mute 3
unmute 3
record_start 1
record_stop 1
stream_start 7
stream_stop 7
play 2
pause 2
seek 2, 250
restart 2
"#;
    let mut host = host();
    let report = run_source(source, &mut host, Limits::default());
    assert_eq!(report.failure, None);
    assert_eq!(report.actions, 28);
    let name = |text: &str| InputRef::Name(text.to_owned());
    assert_eq!(
        host.actions,
        vec![
            ScriptAction::Preview(InputRef::Number(2)),
            ScriptAction::Take {
                input: None,
                effect: TakeEffect::Cut,
                frames: 0
            },
            ScriptAction::Take {
                input: Some(name("Camera 3")),
                effect: TakeEffect::Cut,
                frames: 0
            },
            ScriptAction::Take {
                input: Some(InputRef::Number(4)),
                effect: TakeEffect::Transition(Effect::Wipe),
                frames: 25
            },
            ScriptAction::Take {
                input: None,
                effect: TakeEffect::Transition(Effect::Fade),
                frames: 30
            },
            ScriptAction::Take {
                input: Some(InputRef::Number(1)),
                effect: TakeEffect::Stinger(2),
                frames: 40
            },
            ScriptAction::OverlayOn {
                channel: 1,
                input: InputRef::Number(3)
            },
            ScriptAction::OverlayOff { channel: 1 },
            ScriptAction::TitleOn {
                input: name("Lower third"),
                channel: 2
            },
            ScriptAction::TitleText {
                input: InputRef::Number(4),
                field: 1,
                text: "Score: 3".to_owned()
            },
            ScriptAction::TitleNumber {
                input: InputRef::Number(4),
                field: 2,
                add: false,
                value: 7
            },
            ScriptAction::TitleNumber {
                input: name("Lower third"),
                field: 2,
                add: true,
                value: -1
            },
            ScriptAction::TitleOff(InputRef::Number(4)),
            ScriptAction::FadeToBlack {
                active: true,
                frames: 25
            },
            ScriptAction::FadeToBlack {
                active: false,
                frames: 25
            },
            ScriptAction::PlaylistStart,
            ScriptAction::PlaylistStop,
            ScriptAction::Gain {
                input: InputRef::Number(3),
                millidb: -6000
            },
            ScriptAction::Mute {
                input: InputRef::Number(3),
                muted: true
            },
            ScriptAction::Mute {
                input: InputRef::Number(3),
                muted: false
            },
            ScriptAction::Record {
                recorder: 1,
                start: true
            },
            ScriptAction::Record {
                recorder: 1,
                start: false
            },
            ScriptAction::Stream {
                target: 7,
                start: true
            },
            ScriptAction::Stream {
                target: 7,
                start: false
            },
            ScriptAction::Media {
                input: InputRef::Number(2),
                operation: MediaOperation::Play
            },
            ScriptAction::Media {
                input: InputRef::Number(2),
                operation: MediaOperation::Pause
            },
            ScriptAction::Media {
                input: InputRef::Number(2),
                operation: MediaOperation::Seek(250)
            },
            ScriptAction::Media {
                input: InputRef::Number(2),
                operation: MediaOperation::Restart
            },
        ]
    );
}

#[test]
fn control_flow_variables_and_reads_are_deterministic() {
    let source = "\
let total = 0
let n = 1
while n <= 4
  let total = total + n * 2
  let n = n + 1
end
repeat 2
  preview 3
end
if program == 1 and not (preview != 3)
  title_text 1, 1, \"total=\" + total + \" \" + true
elif program == 2
  stop
else
  cut 4
end
let i = 0
repeat 10
  let i = i + 1
  if i % 3 == 0
    stop
  end
  overlay i, 1
end
";
    let first = {
        let mut host = host();
        let report = run_source(source, &mut host, Limits::default());
        (report, host.actions)
    };
    let second = {
        let mut host = host();
        let report = run_source(source, &mut host, Limits::default());
        (report, host.actions)
    };
    assert_eq!(first, second);
    let (report, actions) = first;
    assert_eq!(report.failure, None);
    assert_eq!(
        actions,
        vec![
            ScriptAction::Preview(InputRef::Number(3)),
            ScriptAction::Preview(InputRef::Number(3)),
            ScriptAction::TitleText {
                input: InputRef::Number(1),
                field: 1,
                text: "total=20 true".to_owned()
            },
            ScriptAction::OverlayOn {
                channel: 1,
                input: InputRef::Number(1)
            },
            ScriptAction::OverlayOn {
                channel: 2,
                input: InputRef::Number(1)
            },
        ]
    );
    assert!(report.instructions > 0);
}

fn failure(source: &str, limits: Limits) -> (FailureKind, Span, String, u32) {
    let mut host = host();
    let report = run_source(source, &mut host, limits);
    let failure = report.failure.expect("the script fails");
    (failure.kind, failure.at, failure.message, report.actions)
}

#[test]
fn syntax_errors_report_line_and_column() {
    for (source, line, column, needle) in [
        ("cut 1\n  cutt 2", 2, 3, "unknown statement cutt"),
        ("let x = (1 + 2", 1, 9, "never closed"),
        ("if x = 1\nend", 1, 6, "use =="),
        ("repeat 3\n  cut 1\n", 3, 1, "has no end"),
        ("end", 1, 1, "unexpected end"),
        (
            "transition swirl, 25",
            1,
            12,
            "unknown transition effect swirl",
        ),
        ("overlay 1", 1, 1, "overlay takes 2 arguments"),
        ("cut 1, 2", 1, 1, "cut takes 0 to 1 argument"),
        ("let if = 3", 1, 5, "reserved"),
        ("title_text 1, 1, \"open", 1, 18, "unterminated"),
        ("preview 2 $", 1, 11, "unexpected character $"),
        ("let x = 99999999999999999999", 1, 9, "too large"),
        ("stop now", 1, 6, "after stop"),
        ("else", 1, 1, "unexpected else"),
    ] {
        let parsed = parse(source).expect_err(source);
        assert_eq!(parsed.kind, FailureKind::Syntax, "{source}");
        assert_eq!(parsed.at, Span::new(line, column), "{source}: {parsed}");
        assert!(parsed.message.contains(needle), "{source}: {parsed}");
        // A run of a script that does not parse does nothing.
        let (kind, at, _, actions) = failure(source, Limits::default());
        assert_eq!((kind, at, actions), (FailureKind::Syntax, parsed.at, 0));
    }
    // Source and nesting bounds are syntax errors too.
    let long = "#".repeat(MAX_SOURCE_BYTES + 1);
    assert!(parse(&long).unwrap_err().message.contains("longer than"));
    let lines = "\n".repeat(fm_script::MAX_SOURCE_LINES + 1);
    assert!(parse(&lines).unwrap_err().message.contains("more than"));
    let deep = format!("{}{}", "if true\n".repeat(17), "end\n".repeat(17));
    assert!(parse(&deep).unwrap_err().message.contains("nest deeper"));
    let parens = format!("let x = {}1{}", "(".repeat(40), ")".repeat(40));
    assert!(parse(&parens).unwrap_err().message.contains("nests deeper"));
}

#[test]
fn runtime_errors_stop_the_run_where_they_happen() {
    for (source, line, needle) in [
        ("cut 1\nlet x = y + 1", 2, "y is not defined"),
        ("let x = 1 / 0", 1, "division by zero"),
        ("let x = 9223372036854775807 + 1", 1, "overflows"),
        ("if 1\nend", 1, "must be true or false"),
        ("let x = 1 == \"1\"", 1, "cannot compare"),
        ("overlay 300, 1", 1, "out of range"),
        ("preview 0", 1, "no input number 0"),
        ("repeat -1\nend", 1, "0 or more"),
        ("let x = true + 1", 1, "cannot combine"),
    ] {
        let (kind, at, message, _) = failure(source, Limits::default());
        assert_eq!(kind, FailureKind::Runtime, "{source}");
        assert_eq!(at.line, line, "{source}");
        assert!(message.contains(needle), "{source}: {message}");
    }
    // Actions accepted before the error are reported.
    let (_, _, _, actions) = failure("cut 1\nlet x = y", Limits::default());
    assert_eq!(actions, 1);
}

#[test]
fn every_budget_is_enforced() {
    // An endless loop exhausts the instruction budget, never the host.
    let (kind, at, message, actions) = failure("while true\nend", Limits::default());
    assert_eq!(kind, FailureKind::Budget(Budget::Instructions));
    assert_eq!(at.line, 1);
    assert!(message.contains("10000 instructions"), "{message}");
    assert_eq!(actions, 0);
    let mut host = host();
    let report = run_source(
        "while true\nend",
        &mut host,
        Limits {
            instructions: 50,
            ..Limits::default()
        },
    );
    assert_eq!(report.instructions, 50);

    let (kind, at, _, actions) = failure(
        "repeat 100\n  preview 1\nend",
        Limits {
            actions: 3,
            ..Limits::default()
        },
    );
    assert_eq!(kind, FailureKind::Budget(Budget::Actions));
    assert_eq!((at.line, actions), (2, 3));

    let (kind, at, _, _) = failure(
        "let a = 1\nlet b = 2\nlet a = 3\nlet c = 4",
        Limits {
            variables: 2,
            ..Limits::default()
        },
    );
    assert_eq!(kind, FailureKind::Budget(Budget::Variables));
    assert_eq!(at.line, 4);

    let (kind, at, _, _) = failure(
        "let s = \"ab\"\nwhile true\n  let s = s + s\nend",
        Limits::default(),
    );
    assert_eq!(kind, FailureKind::Budget(Budget::StringBytes));
    assert_eq!(at.line, 3);
}

#[test]
fn a_refused_action_stops_the_run_at_its_line() {
    let (kind, at, message, actions) =
        failure("cut 1\npreview 2\n\ncut 9\ncut 2", Limits::default());
    assert_eq!(kind, FailureKind::Refused);
    assert_eq!(at, Span::new(4, 1));
    assert_eq!(message, "the show has no input 9");
    assert_eq!(actions, 2);
}

#[test]
fn a_parsed_program_runs_again_unchanged() {
    let program = parse("preview program\ncut preview").unwrap();
    let mut host = host();
    let first = run(&program, &mut host, Limits::default());
    let second = run(&program, &mut host, Limits::default());
    assert_eq!(first, second);
    assert_eq!(
        host.actions,
        vec![
            ScriptAction::Preview(InputRef::Number(1)),
            ScriptAction::Take {
                input: Some(InputRef::Number(1)),
                effect: TakeEffect::Cut,
                frames: 0
            },
            ScriptAction::Preview(InputRef::Number(1)),
            ScriptAction::Take {
                input: Some(InputRef::Number(1)),
                effect: TakeEffect::Cut,
                frames: 0
            },
        ]
    );
}

/// Regression: a file of exactly `MAX_SOURCE_LINES` lines ends with a
/// newline; that newline does not start a refused extra line.
#[test]
fn a_final_newline_does_not_count_as_a_line() {
    let max = fm_script::MAX_SOURCE_LINES;
    parse(&"stop\n".repeat(max)).expect("256 lines with a final newline");
    parse(&"stop\r\n".repeat(max)).expect("256 CRLF lines");
    parse(&"stop\n".repeat(max - 1)).expect("255 lines");
    let over = parse(&format!("{}stop", "stop\n".repeat(max))).unwrap_err();
    assert_eq!(over.at, Span::new(257, 1));
    assert!(over.message.contains("more than"), "{over}");
    let blank = parse(&format!("{}\n", "stop\n".repeat(max))).unwrap_err();
    assert_eq!(blank.at, Span::new(257, 1));
}

/// Regression: `-9223372036854775808` is the smallest number, as
/// documented, while its magnitude alone is out of range; `i64::MIN / -1`
/// and `i64::MIN % -1` overflow instead of panicking.
#[test]
fn the_smallest_number_is_a_literal_and_its_edge_arithmetic_is_checked() {
    let mut host = host();
    let report = run_source(
        "title_text 1, 1, -9223372036854775808",
        &mut host,
        Limits::default(),
    );
    assert_eq!(report.failure, None);
    assert_eq!(
        host.actions,
        vec![ScriptAction::TitleText {
            input: InputRef::Number(1),
            field: 1,
            text: i64::MIN.to_string(),
        }]
    );
    for (source, column) in [
        ("let x = 9223372036854775808", 9),
        ("let x = 1 - 9223372036854775808", 13),
        ("let x = -99999999999999999999", 10),
        ("let x = -9223372036854775809", 10),
    ] {
        let failure = parse(source).expect_err(source);
        assert!(failure.message.contains("too large"), "{source}: {failure}");
        assert_eq!(failure.at, Span::new(1, column), "{source}");
    }
    for source in [
        "let x = -9223372036854775808 / -1",
        "let x = -9223372036854775808 % -1",
        "let x = -(-9223372036854775808)",
        "let x = -9223372036854775808 - 1",
        "let x = -9223372036854775808 * -1",
    ] {
        let (kind, _, message, _) = failure(source, Limits::default());
        assert_eq!(kind, FailureKind::Runtime, "{source}");
        assert!(message.contains("overflows"), "{source}: {message}");
    }
}

/// Regression: parentheses reset the parser's nesting counter, so an
/// operator chain whose first operand is a parenthesized chain (and so on)
/// built a tree about 500 nodes deep within one line, and evaluating it
/// overflowed a 1 MiB stack. The tree's height is now bounded instead.
#[test]
fn a_chain_of_parenthesized_chains_cannot_nest_past_the_bound() {
    fn group(depth: usize) -> String {
        let first = if depth == fm_script::MAX_EXPRESSION_DEPTH {
            "1".to_owned()
        } else {
            format!("({})", group(depth + 1))
        };
        format!(
            "{first}{}",
            "+1".repeat(fm_script::MAX_EXPRESSION_DEPTH - depth)
        )
    }
    let source = format!("let x = {}", group(1));
    assert!(source.len() < MAX_SOURCE_BYTES);
    let failure = parse(&source).unwrap_err();
    assert_eq!(failure.kind, FailureKind::Syntax);
    assert!(failure.message.contains("nests deeper"), "{failure}");

    // The bound itself still parses and runs: 32 levels in one chain, or
    // spread across parentheses.
    let flat = format!(
        "let x = 1{}",
        "+1".repeat(fm_script::MAX_EXPRESSION_DEPTH - 2)
    );
    parse(&flat).expect("a chain at the bound");
    let split = format!(
        "let x = (1{})+1+1",
        "+1".repeat(fm_script::MAX_EXPRESSION_DEPTH - 3)
    );
    parse(&split).expect("a split chain at the bound");
    let over = format!(
        "let x = (1{})+1+1",
        "+1".repeat(fm_script::MAX_EXPRESSION_DEPTH - 2)
    );
    assert!(parse(&over).unwrap_err().message.contains("nests deeper"));
}

/// Regression: every `elif` nested the next one inside it, so a long chain
/// (one `elif` per line, up to the line bound) recursed past the block
/// bound in the parser and the interpreter. The chain is flat now.
#[test]
fn a_long_elif_chain_runs_flat() {
    let source = format!(
        "let x = false\nif x\n{}elif true\n  cut 2\nelse\n  cut 3\nend",
        "elif x\n".repeat(fm_script::MAX_SOURCE_LINES - 7)
    );
    let report = std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(move || {
            let mut host = host();
            let report = run_source(&source, &mut host, Limits::default());
            (report, host.actions)
        })
        .unwrap()
        .join()
        .expect("the run fits a small stack");
    assert_eq!(report.0.failure, None);
    assert_eq!(
        report.1,
        vec![ScriptAction::Take {
            input: Some(InputRef::Number(2)),
            effect: TakeEffect::Cut,
            frames: 0,
        }]
    );
    // The `let`, then each of the 251 conditions tried costs one for its
    // `if` or `elif` and one for its value, then the `cut` and its input.
    assert_eq!(report.0.instructions, 2 + 2 * 251 + 2);
}

/// A program's verbs cover every branch and loop body, whatever a run
/// takes, so they bound what any run of it can ask for (a cue that runs a
/// script needs their permissions up front).
#[test]
fn verbs_cover_every_branch_whatever_a_run_takes() {
    let program = parse(
        "# never taken\n\
         if false\n  mute 1\nelif program == 99\n  title_text 1, 1, \"x\"\nelse\n  cut\nend\n\
         repeat 0\n  preview 2\nend\n\
         while false\n  gain 1, 0\nend\n\
         if false\n  title_add 1, 2, 1\nend\n\
         stop\nrecord_start 1\n",
    )
    .expect("parses");
    assert_eq!(
        program.verbs(),
        vec![
            Verb::Preview,
            Verb::Cut,
            Verb::TitleText,
            Verb::TitleAdd,
            Verb::Gain,
            Verb::Mute,
            Verb::RecordStart
        ]
    );
    // A run only performs the `cut`.
    let mut recorder = host();
    let report = run(&program, &mut recorder, Limits::default());
    assert_eq!(report.failure, None);
    assert_eq!(recorder.actions.len(), 1);
    assert_eq!(parse("let x = 1\n").expect("parses").verbs(), []);
}

/// Runs `program` to completion, resuming after every `wait`, and returns
/// each part's `(frames waited before it, actions it performed)` and the
/// final report. `between` changes the host while a part waits.
fn run_parts(
    program: &fm_script::Program,
    host: &mut Recorder,
    limits: Limits,
    mut between: impl FnMut(&mut Recorder, u32),
) -> (Vec<(u32, usize)>, fm_script::ScriptReport) {
    let mut parts = Vec::new();
    let mut before = host.actions.len();
    let mut report = run(program, host, limits);
    parts.push((0, host.actions.len() - before));
    while let Some(suspension) = report.suspension.clone() {
        assert!(program.accepts(&suspension.state, &limits));
        between(host, suspension.frames);
        before = host.actions.len();
        report = fm_script::resume(program, &suspension.state, host, limits);
        parts.push((suspension.frames, host.actions.len() - before));
    }
    (parts, report)
}

/// RC-011: `wait` suspends a run with plain, resumable state; the parts
/// after it read Program and Preview as they are when they run, and loop
/// counters, branches, and variables carry across.
#[test]
fn a_wait_suspends_the_run_and_resume_continues_it() {
    let program = parse(
        "let n = 0\n\
         repeat 3\n  \
           let n = n + 1\n  \
           if n == 2\n    title_text 4, 1, \"two\"\n    wait 10\n  else\n    preview n\n    wait n\n  end\n\
         end\n\
         while program != 3\n  wait 5\nend\n\
         cut 2\n",
    )
    .expect("parses");
    assert!(program.waits());
    assert_eq!(
        program.verbs(),
        vec![Verb::Preview, Verb::Cut, Verb::TitleText]
    );
    let mut recorder = host();
    let mut polls = 0;
    let (parts, report) = run_parts(
        &program,
        &mut recorder,
        Limits::default(),
        |host, frames| {
            // Program becomes 3 during the second poll of the `while`.
            if frames == 5 {
                polls += 1;
                if polls == 2 {
                    host.program = 3;
                }
            }
        },
    );
    assert_eq!(report.failure, None);
    assert_eq!(report.suspension, None);
    // preview 1, wait 1 | title, wait 10 | preview 3, wait 3 | poll,
    // wait 5 | poll, wait 5 (Program becomes 3) | cut 2.
    assert_eq!(parts, vec![(0, 1), (1, 1), (10, 1), (3, 0), (5, 0), (5, 1)]);
    assert_eq!(report.actions, 4);
    assert_eq!(
        recorder.actions[2],
        ScriptAction::Preview(InputRef::Number(3))
    );
    assert!(!parse("cut 1\n").expect("parses").waits());
}

#[test]
fn waits_are_bounded_and_checked() {
    for (source, needle) in [
        ("wait 0", "1 or more frames"),
        ("wait -2", "1 or more frames"),
        ("wait \"x\"", "number of frames"),
    ] {
        let (kind, _, message, _) = failure(source, Limits::default());
        assert_eq!(kind, FailureKind::Runtime, "{source}");
        assert!(message.contains(needle), "{source}: {message}");
    }
    assert_eq!(parse("wait").unwrap_err().kind, FailureKind::Syntax);
    assert_eq!(parse("let wait = 1").unwrap_err().kind, FailureKind::Syntax);

    // A host that refuses every wait (a cue step) fails the run there.
    let (kind, at, _, actions) = failure(
        "cut 1\nwait 1\ncut 2",
        Limits {
            waits: 0,
            ..Limits::default()
        },
    );
    assert_eq!(kind, FailureKind::Budget(Budget::Waits));
    assert_eq!((at.line, actions), (2, 1));

    // The count and the total frames of the waits are budgets of the run.
    let program = parse("repeat 10\n  wait 100\nend").expect("parses");
    let (parts, report) = run_parts(
        &program,
        &mut host(),
        Limits {
            waits: 4,
            ..Limits::default()
        },
        |_, _| {},
    );
    assert_eq!(parts.len(), 5);
    assert_eq!(
        report.failure.map(|failure| failure.kind),
        Some(FailureKind::Budget(Budget::Waits))
    );
    let (parts, report) = run_parts(
        &program,
        &mut host(),
        Limits {
            wait_frames: 250,
            ..Limits::default()
        },
        |_, _| {},
    );
    assert_eq!(parts.len(), 3);
    assert_eq!(
        report.failure.map(|failure| failure.kind),
        Some(FailureKind::Budget(Budget::WaitFrames))
    );

    // The instruction budget covers the whole run, across its waits.
    let program = parse("while true\n  wait 1\nend").expect("parses");
    let (_, report) = run_parts(
        &program,
        &mut host(),
        Limits {
            instructions: 40,
            ..Limits::default()
        },
        |_, _| {},
    );
    assert_eq!(
        report.failure.map(|failure| failure.kind),
        Some(FailureKind::Budget(Budget::Instructions))
    );
    assert_eq!(report.instructions, 40);
}

#[test]
fn a_saved_state_that_does_not_fit_the_program_is_refused() {
    let program = parse("if true\n  wait 2\nend\ncut 1").expect("parses");
    let report = run(&program, &mut host(), Limits::default());
    let state = report.suspension.expect("waits").state;
    let point = fm_script::ResumePoint {
        statement: 0,
        branch: 0,
    };
    assert_eq!(state.path, vec![point, point]);
    let limits = Limits::default();
    assert!(program.accepts(&state, &limits));
    let other = parse("cut 1\nwait 2").expect("parses");
    assert!(!other.accepts(&state, &limits));
    let report = fm_script::resume(&other, &state, &mut host(), limits);
    assert_eq!(
        report.failure.map(|failure| failure.kind),
        Some(FailureKind::Runtime)
    );
    for broken in [
        fm_script::Suspended {
            path: vec![fm_script::ResumePoint {
                statement: 0,
                branch: 2,
            }],
            ..state.clone()
        },
        fm_script::Suspended {
            waits: 0,
            ..state.clone()
        },
        fm_script::Suspended {
            variables: vec![("if".to_owned(), fm_script::ScriptValue::Int(1))],
            ..state.clone()
        },
        fm_script::Suspended {
            variables: vec![
                ("a".to_owned(), fm_script::ScriptValue::Int(1)),
                ("a".to_owned(), fm_script::ScriptValue::Int(2)),
            ],
            ..state.clone()
        },
    ] {
        assert!(!program.accepts(&broken, &limits), "{broken:?}");
    }
}
