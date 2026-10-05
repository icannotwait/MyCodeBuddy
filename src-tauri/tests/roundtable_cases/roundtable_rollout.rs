//! The product gate stays off, and disabling it does not delete evidence.

use codeg_lib::roundtable::{disable_and_drain, exercise_room, may_start, Rollout};

#[test]
fn fake_room_reaches_terminal_without_prompts_or_tools() {
    let trajectory = exercise_room();
    assert_eq!(trajectory.member_turns, 2);
    assert_eq!(trajectory.moderator_turns, 1);
    assert_eq!(trajectory.phases, 3);
    assert_eq!(trajectory.accepted_messages, 2);
    assert_eq!(trajectory.prompt_bodies, 0);
    assert_eq!(trajectory.tool_calls, 0);
    assert_eq!(trajectory.business_events, 1);
    assert_eq!(trajectory.cleanup_proofs, 1);
    assert!(trajectory.terminal);
}

#[test]
fn disabled_gate_blocks_new_admission_and_keeps_evidence() {
    let mut rollout = Rollout::disabled();
    rollout.evidence = 4;
    rollout.active = 2;
    assert!(!may_start(&rollout));
    rollout.enabled = true;
    assert!(may_start(&rollout));
    disable_and_drain(&mut rollout);
    assert!(!may_start(&rollout));
    assert_eq!(rollout.active, 0);
    assert_eq!(rollout.evidence, 4);
}
