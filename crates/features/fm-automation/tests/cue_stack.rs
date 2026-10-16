//! RC-017: the generic cue stack runs GO, frame waits, auto-follow, and
//! the standby pointer deterministically on frame numbers alone.

use fm_automation::{CuePendingStep, CueStack, CueStackError, CueStep, StackCue};

fn stack() -> CueStack<&'static str> {
    CueStack::new(vec![
        StackCue {
            id: 1,
            steps: vec![
                CueStep::Action("a"),
                CueStep::Wait(10),
                CueStep::Action("b"),
                CueStep::Wait(5),
            ],
            auto_follow: true,
        },
        StackCue {
            id: 2,
            steps: vec![CueStep::Action("c")],
            auto_follow: true,
        },
        StackCue {
            id: 3,
            steps: vec![CueStep::Action("d")],
            auto_follow: false,
        },
        StackCue {
            id: 4,
            steps: vec![CueStep::Action("e")],
            auto_follow: false,
        },
    ])
    .unwrap()
}

fn actions(firing: &fm_automation::CueFiring<&'static str>) -> Vec<&'static str> {
    firing.actions.iter().map(|fired| fired.action).collect()
}

#[test]
fn go_waits_and_auto_follow_land_on_exact_frames() {
    let stack = stack();
    let state = stack.initial_state();
    assert_eq!(state.standby, Some(1));

    let first = stack.go(&state, None, 100).unwrap();
    assert_eq!(actions(&first), vec!["a"]);
    assert_eq!(first.fired, vec![1]);
    assert_eq!(
        first.state.pending,
        Some(CuePendingStep {
            cue: 1,
            step: 2,
            due_frame: 110,
        })
    );
    assert_eq!(first.state.standby, Some(2));
    assert_eq!(
        stack.advance(&first.state, 109),
        Err(CueStackError::NothingDue { frame: 109 })
    );

    let second = stack.advance(&first.state, 110).unwrap();
    assert_eq!(actions(&second), vec!["b"]);
    // A trailing wait before an auto-follow keeps the cue pending.
    assert_eq!(second.state.pending.unwrap().due_frame, 115);
    assert_eq!(second.state.pending.unwrap().step, 4);

    // Cue 1 follows on to cue 2, which follows on to cue 3 on one frame.
    let third = stack.advance(&second.state, 115).unwrap();
    assert_eq!(actions(&third), vec!["c", "d"]);
    assert_eq!(third.fired, vec![2, 3]);
    assert_eq!(third.state.live, Some(3));
    assert_eq!(third.state.standby, Some(4));
    assert_eq!(third.state.played, vec![1, 2, 3]);
    assert_eq!(third.state.pending, None);
}

#[test]
fn standby_moves_and_go_supersedes_a_waiting_cue() {
    let stack = stack();
    let state = stack.go(&stack.initial_state(), None, 0).unwrap().state;
    assert!(state.pending.is_some());
    let jumped = stack.go(&state, Some(4), 3).unwrap();
    assert_eq!(actions(&jumped), vec!["e"]);
    assert_eq!(jumped.state.pending, None);
    assert_eq!(jumped.state.standby, None);
    assert_eq!(
        stack.go(&jumped.state, None, 4),
        Err(CueStackError::NothingOnStandby)
    );
    let back = stack.back(&jumped.state);
    assert_eq!(back.standby, Some(4));
    let back = stack.back(&stack.back(&stack.back(&stack.back(&back))));
    assert_eq!(back.standby, Some(1));
    assert_eq!(stack.skip(&back).standby, Some(2));
    assert_eq!(stack.standby(&back, 9), Err(CueStackError::UnknownCue(9)));
    assert_eq!(stack.standby(&back, 3).unwrap().standby, Some(3));
    assert_eq!(stack.reset(), stack.initial_state());
}

#[test]
fn reconcile_carries_the_run_state_across_an_edit() {
    let stack = stack();
    let state = stack.go(&stack.initial_state(), None, 0).unwrap().state;
    let previous = [1, 2, 3, 4];
    let edited = CueStack::new(vec![
        StackCue {
            id: 1,
            steps: vec![
                CueStep::Action("a"),
                CueStep::Wait(10),
                CueStep::Action("b"),
            ],
            auto_follow: false,
        },
        StackCue {
            id: 3,
            steps: vec![CueStep::Action("d")],
            auto_follow: false,
        },
    ])
    .unwrap();
    let carried = edited.reconcile(&previous, &state);
    // Standby cue 2 was removed: the next surviving cue takes its place.
    assert_eq!(carried.standby, Some(3));
    assert_eq!(carried.pending.unwrap().step, 2);
    assert_eq!(carried.played, vec![1]);

    let failed = stack.fail(&state, 1, 2);
    assert_eq!(failed.fault, Some((1, 2)));
    assert_eq!(failed.pending, None);
    assert!(matches!(
        CueStack::new(vec![StackCue {
            id: 1,
            steps: vec![CueStep::<()>::Wait(0)],
            auto_follow: false,
        }]),
        Err(CueStackError::ZeroWait { cue: 1 })
    ));
}

fn single_steps(ids: &[u64]) -> CueStack<&'static str> {
    CueStack::new(
        ids.iter()
            .map(|id| StackCue {
                id: *id,
                steps: vec![CueStep::Action("x")],
                auto_follow: false,
            })
            .collect(),
    )
    .unwrap()
}

#[test]
fn reconcile_puts_the_first_cue_added_after_the_end_on_standby() {
    let stack = single_steps(&[1, 2]);
    let mut state = stack.initial_state();
    for _ in 0..2 {
        state = stack.go(&state, None, 0).unwrap().state;
    }
    assert_eq!(state.standby, None, "the list ran off its end");

    // A cue inserted before the end does not reopen the list; the first cue
    // added after the old last cue goes on standby.
    let edited = single_steps(&[5, 1, 2, 3, 4]);
    let carried = edited.reconcile(&[1, 2], &state);
    assert_eq!(carried.standby, Some(3));
    assert_eq!(carried.live, Some(2));
    assert_eq!(carried.played, vec![1, 2]);

    // Re-saving the same cues keeps the list played out.
    assert_eq!(stack.reconcile(&[1, 2], &state).standby, None);
    // An empty list that gains cues puts the first one on standby.
    let empty = CueStack::<&'static str>::new(Vec::new()).unwrap();
    let carried = stack.reconcile(&[], &empty.initial_state());
    assert_eq!(carried.standby, Some(1));
}

#[test]
fn reconcile_moves_standby_to_the_cue_replacing_it_at_its_position() {
    let stack = single_steps(&[1, 2, 3]);
    let state = stack.go(&stack.initial_state(), None, 0).unwrap().state;
    assert_eq!(state.standby, Some(2));

    // Cue 2 is replaced by new cue 9 at the same position.
    let carried = single_steps(&[1, 9, 3]).reconcile(&[1, 2, 3], &state);
    assert_eq!(carried.standby, Some(9));
    // Removed outright: the next surviving cue takes its place.
    let carried = single_steps(&[1, 3, 9]).reconcile(&[1, 2, 3], &state);
    assert_eq!(carried.standby, Some(3));
    // The standby cue was the last and is removed: a cue added after the
    // end takes its place.
    let last = stack.standby(&state, 3).unwrap();
    let carried = single_steps(&[1, 2, 8]).reconcile(&[1, 2, 3], &last);
    assert_eq!(carried.standby, Some(8));
    let carried = single_steps(&[1, 2]).reconcile(&[1, 2, 3], &last);
    assert_eq!(carried.standby, None);
}
