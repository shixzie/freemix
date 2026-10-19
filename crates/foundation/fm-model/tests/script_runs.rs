//! Several waiting script runs (schema 60, RC-011): the run list's shape.

use fm_command::FaultDetail;
use fm_model::{
    MAX_SCRIPT_RUNS, Script, ScriptFault, ScriptRun, ScriptRunState, validate_script_run,
};
use fm_script::{FailureKind, ResumePoint, Span, Suspended};
use fm_types::FrameRate;

fn scripts(count: u64) -> Vec<Script> {
    (1..=count)
        .map(|id| Script {
            id,
            name: format!("Script {id}"),
            source: "wait 5\ncut 1\n".to_owned(),
        })
        .collect()
}

fn run(run: u64, script: u64) -> ScriptRun {
    ScriptRun {
        run,
        script,
        started_frame: 10,
        due_frame: 15,
        state: Suspended {
            path: vec![ResumePoint {
                statement: 0,
                branch: 0,
            }],
            variables: Vec::new(),
            instructions: 1,
            actions: 0,
            waits: 1,
            wait_frames: 5,
        },
    }
}

fn fault(run: u64, script: u64) -> ScriptFault {
    ScriptFault {
        run,
        script,
        frame: 20,
        failure: FailureKind::Runtime,
        at: Span::new(2, 1),
        detail: FaultDetail::new(None, "the show has no input number 1"),
    }
}

fn check(state: &ScriptRunState, scripts: &[Script]) -> bool {
    validate_script_run(state, scripts, FrameRate::new(25, 1).unwrap()).is_ok()
}

#[test]
fn several_runs_of_different_scripts_wait_together() {
    let scripts = scripts(10);
    let state = ScriptRunState {
        last_run: 5,
        runs: vec![run(1, 3), run(4, 1)],
        faults: vec![fault(5, 2)],
    };
    assert!(check(&state, &scripts));
    assert_eq!(state.run_of(1).map(|found| found.run), Some(4));
    assert_eq!(state.fault_of(2).map(|found| found.run), Some(5));
    assert_eq!(state.script_of(1), Some(3));
    assert_eq!(state.script_of(5), Some(2));
    assert_eq!(state.script_of(2), None);
    assert_eq!(state.scripts(), vec![1, 2, 3]);
    // The earliest due goes first, the lowest run on a tie.
    assert_eq!(state.next_due().map(|found| found.run), Some(1));
    let mut later = state.clone();
    later.runs[0].due_frame = 16;
    assert_eq!(later.next_due().map(|found| found.run), Some(4));

    let full = ScriptRunState {
        last_run: MAX_SCRIPT_RUNS as u64,
        runs: (1..=MAX_SCRIPT_RUNS as u64).map(|id| run(id, id)).collect(),
        faults: Vec::new(),
    };
    assert!(check(&full, &scripts));
}

#[test]
fn a_run_list_out_of_shape_is_invalid() {
    let scripts = scripts(10);
    let valid = ScriptRunState {
        last_run: 3,
        runs: vec![run(1, 1), run(2, 2)],
        faults: vec![fault(3, 3)],
    };
    assert!(check(&valid, &scripts));
    let too_many = ScriptRunState {
        last_run: 9,
        runs: (1..=9).map(|id| run(id, id)).collect(),
        faults: Vec::new(),
    };
    let broken = [
        // A run id past the last one issued, or zero.
        ScriptRunState {
            last_run: 2,
            ..valid.clone()
        },
        ScriptRunState {
            runs: vec![run(0, 1), run(2, 2)],
            ..valid.clone()
        },
        // Out of order.
        ScriptRunState {
            runs: vec![run(2, 2), run(1, 1)],
            ..valid.clone()
        },
        // The same run id twice, also across a run and a fault.
        ScriptRunState {
            faults: vec![fault(2, 3)],
            ..valid.clone()
        },
        // The same script twice.
        ScriptRunState {
            runs: vec![run(1, 1), run(2, 1)],
            ..valid.clone()
        },
        ScriptRunState {
            faults: vec![fault(3, 2)],
            ..valid.clone()
        },
        // An unknown script.
        ScriptRunState {
            faults: vec![fault(3, 11)],
            ..valid.clone()
        },
        too_many,
    ];
    for state in broken {
        assert!(!check(&state, &scripts), "{state:?}");
    }
}
