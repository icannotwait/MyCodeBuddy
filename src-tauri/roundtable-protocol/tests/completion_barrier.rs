//! In-memory completion barrier.
//!
//! The barrier waits outside the gate. A prompt response is not a completed
//! turn, and this file does not prove the later product gate.

#[path = "support/mod.rs"]
#[allow(dead_code)]
mod support;

use std::str::FromStr;

use serde_json::json;

use roundtable_protocol::{
    canonical_bytes, close_mcp_admission, complete_turn, late_submit, refresh_barrier,
    submit_candidate, AcpUpdate, Actor, ActorEffect, BindingLedger, CandidateState, CompletionGate,
    CompletionState, CompletionWait, DecisionKind, ErrorCode, FinishKind, GateTraceEvent,
    HandlerId, PhaseKind, ResultScope, Seq, SubmissionId, SubmissionState, UpdateDisposition,
    VisibleAliases,
};

#[test]
fn response_waits_for_prior_ingress_and_admitted_tools() {
    let fence = fence_for("attempt-a");
    let handler_id = HandlerId::new("admitted-tool");
    let mut state = CompletionState::open(fence.clone());
    let mut ledger = BindingLedger::new();
    let origin = fence.clone();
    state.apply_update(matching(&origin, 1, false), &mut ledger);
    state.admit_handler(handler_id.clone()).expect("mcp open");
    state.set_normal_finish();
    let receipt = sealed_candidate();
    assert_eq!(receipt.state, CandidateState::Staged);
    state
        .stage_candidate(receipt.clone())
        .expect("one staged candidate");
    state.note_prompt_response(3);

    let barrier = close_mcp_admission(&mut state, 3);
    let pending = &state;
    assert!(barrier.pending_tools.contains(&handler_id));
    assert!(barrier.mcp_closed);
    assert!(complete_turn(pending, &barrier).is_err());
    let blocked = complete_turn(&state, &barrier).expect_err("still pending");
    assert_eq!(blocked.code, ErrorCode::InvalidState);
    assert_eq!(blocked.details.reason.as_deref(), Some("barrier_pending"));

    let late = late_submit(&state, &HandlerId::new("never-admitted")).expect_err("not admitted");
    assert_eq!(late.code, ErrorCode::InvalidState);
    assert_eq!(
        late.details.reason.as_deref(),
        Some(roundtable_protocol::InternalReason::AttemptClosed.as_str())
    );
    assert!(ErrorCode::parse("attempt_closed").is_none());
    assert!(state.admit_handler(HandlerId::new("after-close")).is_err());

    state.finish_handler(&handler_id);
    let mut after_tool = barrier.clone();
    refresh_barrier(&state, &mut after_tool);
    assert!(!after_tool.pending_tools.contains(&handler_id));
    assert!(complete_turn(&state, &after_tool).is_err());

    state.apply_update(matching(&origin, 2, false), &mut ledger);
    let mut still_short = after_tool.clone();
    refresh_barrier(&state, &mut still_short);
    assert!(complete_turn(&state, &still_short).is_err());
    state.apply_update(matching(&origin, 3, false), &mut ledger);
    let mut converged = still_short;
    refresh_barrier(&state, &mut converged);
    assert!(converged.pending_tools.is_empty());
    assert!(converged.ingress_applied >= converged.ingress_watermark);

    let mut no_seal = state.clone();
    no_seal.clear_candidate();
    let missing = complete_turn(&no_seal, &converged).expect_err("no seal");
    assert_eq!(missing.details.reason.as_deref(), Some("no_submission"));
    let mut cancelled = state.clone();
    cancelled.set_finish(FinishKind::Cancelled);
    let not_normal = complete_turn(&cancelled, &converged).expect_err("not normal");
    assert_eq!(
        not_normal.details.reason.as_deref(),
        Some("not_normal_finish")
    );

    let done = complete_turn(&state, &converged).expect("normal seal and barrier");
    assert_eq!(done.finish_reason, "normal");
    assert_ne!(done.finish_reason, "accepted");
    assert_eq!(done.fence, fence);
    assert_eq!(done.ingress_watermark, Seq(3));
    assert!(done.tool_barrier.drained);
    assert_eq!(
        done.candidate_id.as_deref(),
        Some(receipt.candidate_id.as_str())
    );

    let trailing = AcpUpdate {
        seq: 4,
        attempt_id: support::id("next-attempt"),
        binding_id: fence.binding_id,
        incarnation: support::id("other-incarnation"),
        cancelled: true,
    };
    assert_eq!(
        state.apply_update(trailing, &mut ledger),
        UpdateDisposition::Retired
    );
    assert!(state.binding_retired());
    assert!(!ledger.reusable(&fence.binding_id));
    let reused = CompletionState::claim(&ledger, &fence.binding_id).expect_err("retired");
    assert_eq!(reused.code, ErrorCode::InvalidState);
    assert_eq!(reused.details.reason.as_deref(), Some("binding_retired"));
    let fresh = support::id("binding-fresh");
    assert!(CompletionState::claim(&ledger, &fresh).is_ok());
}

#[test]
fn actor_remains_responsive_while_completion_barrier_waits() {
    let fence = fence_for("attempt-actor");
    let handler_id = HandlerId::new("waiting-tool");
    let mut state = CompletionState::open(fence);
    let mut ledger = BindingLedger::new();
    state.admit_handler(handler_id.clone()).unwrap();
    state.note_prompt_response(1);
    let mut gate = CompletionGate::new();
    let mut wait = CompletionWait::start(&mut gate, &mut state, 1);
    assert!(wait.is_waiting());
    assert!(!wait.gate_held());
    assert!(!gate.held());
    assert!(!wait.waited_inside_gate());
    assert!(wait.barrier().pending_tools.contains(&handler_id));
    assert_eq!(
        wait.trace(),
        &[
            GateTraceEvent::Acquired,
            GateTraceEvent::ClosedMcp,
            GateTraceEvent::CapturedHandlers,
            GateTraceEvent::Released,
        ]
    );
    assert!(wait.poll(&state));
    assert_eq!(wait.trace().last(), Some(&GateTraceEvent::Waited));
    assert!(!wait.waited_inside_gate());

    let mut actor = Actor::new();
    assert!(!actor.gate_held());
    actor.request_reply(handler_id.clone());
    assert_eq!(
        actor.handle_one(),
        Some(ActorEffect::Replied(handler_id.clone()))
    );
    actor.request_stop();
    assert_eq!(actor.handle_one(), Some(ActorEffect::Stopped));
    assert_eq!(actor.stops_handled(), 1);
    assert_eq!(actor.reply_count(), 1);
    assert!(wait.is_waiting());
    assert!(!gate.held());

    state.finish_handler(&handler_id);
    assert!(wait.poll(&state));
    let origin = state.fence().clone();
    state.apply_update(matching(&origin, 1, false), &mut ledger);
    assert!(!wait.poll(&state));
    assert!(!wait.is_waiting());
    assert!(!wait.waited_inside_gate());
    assert!(!gate.held());
}

fn matching(fence: &roundtable_protocol::Fence, seq: u64, cancelled: bool) -> AcpUpdate {
    AcpUpdate {
        seq,
        attempt_id: fence.attempt_id,
        binding_id: fence.binding_id,
        incarnation: fence.incarnation,
        cancelled,
    }
}

fn fence_for(label: &str) -> roundtable_protocol::Fence {
    let mut fence = support::fence(1);
    fence.attempt_id = support::id(label);
    fence.binding_id = support::id(&format!("binding-{label}"));
    fence.incarnation = support::id(&format!("incarnation-{label}"));
    fence
}

fn sealed_candidate() -> roundtable_protocol::CandidateReceipt {
    let scope = ResultScope {
        phase_kind: PhaseKind::Proposal,
        speaker_id: support::id("speaker-self"),
        aliases: VisibleAliases::default(),
        mandatory_targets: Vec::new(),
        published: roundtable_protocol::PublishedHistory::default(),
        quota_bytes: 8 * 1024,
    };
    let raw = canonical_bytes(&json!({
        "kind": "proposal",
        "summary": "成员摘要",
        "claims": [{
            "local_key": "c0",
            "text": "claim",
            "evidence_aliases": [],
            "confidence": "low"
        }]
    }))
    .unwrap();
    let decision = submit_candidate(
        &SubmissionState::open(),
        &SubmissionId::from_str("seal-1").unwrap(),
        &raw,
        &scope,
    );
    match decision.outcome {
        DecisionKind::Staged(receipt) => receipt,
        other => panic!("expected one staged candidate, got {other:?}"),
    }
}
