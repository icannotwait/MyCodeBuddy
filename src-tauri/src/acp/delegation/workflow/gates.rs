//! Task / Final execution-gate evaluator (A7 + B3 + B13).
//!
//! Pure function over pre-loaded evidence. Document gates use
//! `settle_workflow_gate_core`; execution gates are projected only.

use crate::acp::delegation::card_summary::{ReviewVerdict, WorkStatus};
use crate::acp::delegation::workflow::CompletionOutcome;
use crate::db::entities::delegation_task_run::CompletionState;

/// Which execution gate is being evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionGateKind {
    Task,
    Final,
}

/// Terminal / non-terminal classification for gate evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalRunStatus {
    Completed,
    Failed,
    Canceled,
    /// Reserving / running / unknown — not a terminal pass.
    NonTerminal,
}

/// Latest terminal (or non-terminal) run + binding fields needed for A7/B3/B13.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionGateRunEvidence {
    pub task_id: String,
    pub generation: i64,
    pub status: TerminalRunStatus,
    pub completion_protocol_version: i64,
    pub completion_state: Option<CompletionState>,
    pub completion_outcome: Option<CompletionOutcome>,
    pub completion_evidence_validated: bool,
    pub summary_validated: bool,
    /// Implementer / Final fixer work status from validated card summary.
    pub work_status: Option<WorkStatus>,
    /// Reviewer verdict from validated card summary.
    pub review_verdict: Option<ReviewVerdict>,
    /// Artifact digest from the run-binding row (B3).
    pub artifact_digest: Option<String>,
    /// Reviewer binding: exact implementer/fixer task_id under review (B13).
    pub reviewed_task_id: Option<String>,
    /// Informational only (B13); not authoritative for pass/fail.
    pub reviewed_implementer_generation: Option<i64>,
}

/// Evidence for one reviewer required by the normalized policy route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredReviewerEvidence {
    pub node_id: String,
    pub evidence: Option<ExecutionGateRunEvidence>,
}

/// Input bundle for `evaluate_execution_gate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionGateInput {
    pub kind: ExecutionGateKind,
    /// Latest terminal Task implementer, or Final fixer when a fix cycle exists.
    pub implementer_or_fixer: Option<ExecutionGateRunEvidence>,
    /// Latest evidence for every reviewer required by the authoritative route.
    pub required_reviewers: Vec<RequiredReviewerEvidence>,
    /// Optional workspace/branch tip digest for Final first-pass coverage.
    /// When set, reviewer `artifact_digest` must match.
    pub branch_tip_digest: Option<String>,
}

/// Why an execution gate passed or failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionGateReason {
    Passed,
    MissingImplementer,
    ImplementerNotTerminalPass,
    MissingReviewer,
    ReviewerNotTerminalPass,
    /// B13: `reviewed_task_id` ≠ latest terminal implementer/fixer `task_id`.
    ReviewerDoesNotCoverLatestImplementer,
    /// B3: both sides present digests and they differ.
    ArtifactDigestMismatch,
}

/// Result of evaluating a Task/Final execution gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionGateEval {
    pub passed: bool,
    pub reason: ExecutionGateReason,
    /// Required reviewer that produced `reason`; never projected as free text.
    pub reviewer_node_id: Option<String>,
}

/// Evaluate Task/Final execution-gate readiness (A7 + B3 + B13).
///
/// - Implementer/fixer terminal pass: completed + validated summary with
///   `done` / `done_with_concerns`.
/// - Reviewer terminal pass: completed + validated summary with
///   `approve` / `approve_with_minors`, **and** B13 exact `reviewed_task_id`
///   match against the latest implementer/fixer (when one exists), **and**
///   non-empty B3 digest match against the producer/fixer artifact.
/// - `reviewed_implementer_generation` is informational only.
/// - Final first-pass (no fixer terminal): reviewer terminal completed with
///   validated approve summary and non-empty task_id.
pub fn evaluate_execution_gate(input: &ExecutionGateInput) -> ExecutionGateEval {
    match input.kind {
        ExecutionGateKind::Task => evaluate_task_gate(input),
        ExecutionGateKind::Final => evaluate_final_gate(input),
    }
}

fn evaluate_task_gate(input: &ExecutionGateInput) -> ExecutionGateEval {
    let Some(impl_ev) = input.implementer_or_fixer.as_ref() else {
        return fail(ExecutionGateReason::MissingImplementer);
    };
    if !implementer_terminal_pass(impl_ev) {
        return fail(ExecutionGateReason::ImplementerNotTerminalPass);
    }
    if non_empty_digest(impl_ev.artifact_digest.as_deref()).is_none() {
        return fail(ExecutionGateReason::ArtifactDigestMismatch);
    }
    if input.required_reviewers.is_empty() {
        return fail(ExecutionGateReason::MissingReviewer);
    }
    for required in &input.required_reviewers {
        let Some(rev) = required.evidence.as_ref() else {
            return fail_for_reviewer(ExecutionGateReason::MissingReviewer, &required.node_id);
        };
        if !reviewer_verdict_pass(rev) {
            return fail_for_reviewer(
                ExecutionGateReason::ReviewerNotTerminalPass,
                &required.node_id,
            );
        }
        if let Err(reason) = reviewer_covers_implementer(rev, impl_ev) {
            return fail_for_reviewer(reason, &required.node_id);
        }
    }
    pass()
}

fn evaluate_final_gate(input: &ExecutionGateInput) -> ExecutionGateEval {
    if input.required_reviewers.len() != 1 {
        return fail(ExecutionGateReason::MissingReviewer);
    }
    let required = &input.required_reviewers[0];

    // When a Final fixer terminal exists, gate requires fixer pass + reviewer
    // covering that exact fixer run (same as Task pair).
    if let Some(fixer) = input.implementer_or_fixer.as_ref() {
        if !implementer_terminal_pass(fixer) {
            return fail(ExecutionGateReason::ImplementerNotTerminalPass);
        }
        if non_empty_digest(fixer.artifact_digest.as_deref()).is_none() {
            return fail(ExecutionGateReason::ArtifactDigestMismatch);
        }
        let Some(rev) = required.evidence.as_ref() else {
            return fail_for_reviewer(ExecutionGateReason::MissingReviewer, &required.node_id);
        };
        if !reviewer_verdict_pass(rev) {
            return fail_for_reviewer(
                ExecutionGateReason::ReviewerNotTerminalPass,
                &required.node_id,
            );
        }
        if let Err(reason) = reviewer_covers_implementer(rev, fixer) {
            return fail_for_reviewer(reason, &required.node_id);
        }
        return pass();
    }

    // First-pass Final: no fixer/implementer terminal.
    // Require non-empty terminal evidence with validated approve **and**
    // non-empty artifact coverage (no empty-evidence pass).
    let Some(rev) = required.evidence.as_ref() else {
        return fail_for_reviewer(ExecutionGateReason::MissingReviewer, &required.node_id);
    };
    if rev.task_id.trim().is_empty() {
        return fail_for_reviewer(
            ExecutionGateReason::ReviewerNotTerminalPass,
            &required.node_id,
        );
    }
    if !reviewer_verdict_pass(rev) {
        return fail_for_reviewer(
            ExecutionGateReason::ReviewerNotTerminalPass,
            &required.node_id,
        );
    }
    if let Err(reason) = final_first_pass_coverage(rev, input.branch_tip_digest.as_deref()) {
        return fail_for_reviewer(reason, &required.node_id);
    }
    pass()
}

/// Final first-pass (no implementer/fixer): require non-empty reviewer
/// `artifact_digest`. When `branch_tip` is known, digests must match.
fn final_first_pass_coverage(
    rev: &ExecutionGateRunEvidence,
    branch_tip: Option<&str>,
) -> Result<(), ExecutionGateReason> {
    let rev_digest = non_empty_digest(rev.artifact_digest.as_deref());
    let Some(rev_digest) = rev_digest else {
        // No empty-evidence pass: digest required when no implementer/fixer.
        return Err(ExecutionGateReason::ArtifactDigestMismatch);
    };
    if let Some(tip) = branch_tip.map(str::trim).filter(|s| !s.is_empty()) {
        if rev_digest != tip {
            return Err(ExecutionGateReason::ArtifactDigestMismatch);
        }
    }
    Ok(())
}

fn implementer_terminal_pass(ev: &ExecutionGateRunEvidence) -> bool {
    if !matches!(ev.status, TerminalRunStatus::Completed) {
        return false;
    }
    if ev.completion_protocol_version == 2 {
        return ev.completion_state == Some(CompletionState::Resolved)
            && ev.completion_evidence_validated
            && matches!(
                ev.completion_outcome,
                Some(CompletionOutcome::Done | CompletionOutcome::DoneWithConcerns)
            );
    }
    if !ev.summary_validated {
        return false;
    }
    matches!(
        ev.work_status,
        Some(WorkStatus::Done) | Some(WorkStatus::DoneWithConcerns)
    )
}

fn reviewer_verdict_pass(ev: &ExecutionGateRunEvidence) -> bool {
    if !matches!(ev.status, TerminalRunStatus::Completed) {
        return false;
    }
    if ev.completion_protocol_version == 2 {
        return ev.completion_state == Some(CompletionState::Resolved)
            && ev.completion_evidence_validated
            && matches!(
                ev.completion_outcome,
                Some(CompletionOutcome::Approve | CompletionOutcome::ApproveWithMinors)
            );
    }
    if !ev.summary_validated {
        return false;
    }
    matches!(
        ev.review_verdict,
        Some(ReviewVerdict::Approve) | Some(ReviewVerdict::ApproveWithMinors)
    )
}

/// B13 exact task_id coverage + B3 digest rules.
///
/// B3:
/// - implementer has non-empty digest and reviewer missing/empty → fail
/// - both non-empty and differ → fail
/// - producer/fixer digest is required by the caller
fn reviewer_covers_implementer(
    rev: &ExecutionGateRunEvidence,
    impl_ev: &ExecutionGateRunEvidence,
) -> Result<(), ExecutionGateReason> {
    // B13: generation is informational; task_id is authoritative.
    match rev.reviewed_task_id.as_deref() {
        Some(id) if id == impl_ev.task_id => {}
        _ => return Err(ExecutionGateReason::ReviewerDoesNotCoverLatestImplementer),
    }

    let impl_digest = non_empty_digest(impl_ev.artifact_digest.as_deref());
    let rev_digest = non_empty_digest(rev.artifact_digest.as_deref());
    match (impl_digest, rev_digest) {
        (Some(a), Some(b)) if a != b => {
            return Err(ExecutionGateReason::ArtifactDigestMismatch);
        }
        (Some(_), None) => {
            // Implementer recorded a digest; reviewer must carry the same coverage.
            return Err(ExecutionGateReason::ArtifactDigestMismatch);
        }
        (None, Some(_)) => {
            // Reviewer digest present but implementer absent → fail closed.
            return Err(ExecutionGateReason::ArtifactDigestMismatch);
        }
        // Equal non-empty digests pass. The caller rejects an empty producer.
        _ => {}
    }

    let _ = rev.reviewed_implementer_generation; // informational only
    Ok(())
}

fn non_empty_digest(d: Option<&str>) -> Option<&str> {
    d.map(str::trim).filter(|s| !s.is_empty())
}

fn pass() -> ExecutionGateEval {
    ExecutionGateEval {
        passed: true,
        reason: ExecutionGateReason::Passed,
        reviewer_node_id: None,
    }
}

fn fail(reason: ExecutionGateReason) -> ExecutionGateEval {
    ExecutionGateEval {
        passed: false,
        reason,
        reviewer_node_id: None,
    }
}

fn fail_for_reviewer(reason: ExecutionGateReason, node_id: &str) -> ExecutionGateEval {
    ExecutionGateEval {
        passed: false,
        reason,
        reviewer_node_id: Some(node_id.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Tests (B10 owned by Task 4 — A7 / B3 / B13)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn impl_done(task_id: &str, generation: i64, digest: Option<&str>) -> ExecutionGateRunEvidence {
        ExecutionGateRunEvidence {
            task_id: task_id.into(),
            generation,
            status: TerminalRunStatus::Completed,
            completion_protocol_version: 1,
            completion_state: None,
            completion_outcome: None,
            completion_evidence_validated: false,
            summary_validated: true,
            work_status: Some(WorkStatus::Done),
            review_verdict: None,
            artifact_digest: digest.map(|s| s.into()),
            reviewed_task_id: None,
            reviewed_implementer_generation: None,
        }
    }

    fn impl_concerns(task_id: &str) -> ExecutionGateRunEvidence {
        let mut e = impl_done(task_id, 1, Some("abc"));
        e.work_status = Some(WorkStatus::DoneWithConcerns);
        e
    }

    fn impl_blocked(task_id: &str) -> ExecutionGateRunEvidence {
        let mut e = impl_done(task_id, 1, Some("abc"));
        e.work_status = Some(WorkStatus::Blocked);
        e
    }

    fn impl_needs_context(task_id: &str) -> ExecutionGateRunEvidence {
        let mut e = impl_done(task_id, 1, Some("abc"));
        e.work_status = Some(WorkStatus::NeedsContext);
        e
    }

    fn rev_approve(
        task_id: &str,
        reviewed: &str,
        reviewed_gen: Option<i64>,
        digest: Option<&str>,
    ) -> ExecutionGateRunEvidence {
        ExecutionGateRunEvidence {
            task_id: task_id.into(),
            generation: 1,
            status: TerminalRunStatus::Completed,
            completion_protocol_version: 1,
            completion_state: None,
            completion_outcome: None,
            completion_evidence_validated: false,
            summary_validated: true,
            work_status: None,
            review_verdict: Some(ReviewVerdict::Approve),
            artifact_digest: digest.map(|s| s.into()),
            reviewed_task_id: Some(reviewed.into()),
            reviewed_implementer_generation: reviewed_gen,
        }
    }

    fn rev_minors(task_id: &str, reviewed: &str, digest: Option<&str>) -> ExecutionGateRunEvidence {
        let mut e = rev_approve(task_id, reviewed, Some(1), digest);
        e.review_verdict = Some(ReviewVerdict::ApproveWithMinors);
        e
    }

    fn rev_changes(task_id: &str, reviewed: &str) -> ExecutionGateRunEvidence {
        let mut e = rev_approve(task_id, reviewed, Some(1), Some("abc"));
        e.review_verdict = Some(ReviewVerdict::RequestChanges);
        e
    }

    fn rev_block(task_id: &str, reviewed: &str) -> ExecutionGateRunEvidence {
        let mut e = rev_approve(task_id, reviewed, Some(1), Some("abc"));
        e.review_verdict = Some(ReviewVerdict::Block);
        e
    }

    // ---- A7 implementer / reviewer pass matrix ----

    fn task_input(
        impl_ev: Option<ExecutionGateRunEvidence>,
        rev: Option<ExecutionGateRunEvidence>,
    ) -> ExecutionGateInput {
        ExecutionGateInput {
            kind: ExecutionGateKind::Task,
            implementer_or_fixer: impl_ev,
            required_reviewers: vec![RequiredReviewerEvidence {
                node_id: "task-reviewer".into(),
                evidence: rev,
            }],
            branch_tip_digest: None,
        }
    }

    fn final_input(
        impl_ev: Option<ExecutionGateRunEvidence>,
        rev: Option<ExecutionGateRunEvidence>,
        tip: Option<&str>,
    ) -> ExecutionGateInput {
        ExecutionGateInput {
            kind: ExecutionGateKind::Final,
            implementer_or_fixer: impl_ev,
            required_reviewers: vec![RequiredReviewerEvidence {
                node_id: "final-reviewer".into(),
                evidence: rev,
            }],
            branch_tip_digest: tip.map(|s| s.into()),
        }
    }

    fn routed_task_input(
        producer: ExecutionGateRunEvidence,
        reviewers: Vec<(&str, Option<ExecutionGateRunEvidence>)>,
    ) -> ExecutionGateInput {
        ExecutionGateInput {
            kind: ExecutionGateKind::Task,
            implementer_or_fixer: Some(producer),
            required_reviewers: reviewers
                .into_iter()
                .map(|(node_id, evidence)| RequiredReviewerEvidence {
                    node_id: node_id.into(),
                    evidence,
                })
                .collect(),
            branch_tip_digest: None,
        }
    }

    #[test]
    fn task6_normal_route_requires_its_one_reviewer() {
        let eval = evaluate_execution_gate(&routed_task_input(
            impl_done("impl-1", 1, Some("digest-1")),
            vec![(
                "task-1-codex-reviewer",
                Some(rev_approve(
                    "codex-review-1",
                    "impl-1",
                    Some(1),
                    Some("digest-1"),
                )),
            )],
        ));

        assert!(eval.passed);
        assert_eq!(eval.reason, ExecutionGateReason::Passed);
        assert_eq!(eval.reviewer_node_id, None);
    }

    #[test]
    fn task6_high_route_requires_both_reviewers_over_same_artifact() {
        let eval = evaluate_execution_gate(&routed_task_input(
            impl_done("impl-1", 1, Some("digest-1")),
            vec![
                (
                    "task-1-codex-reviewer",
                    Some(rev_approve(
                        "codex-review-1",
                        "impl-1",
                        Some(1),
                        Some("digest-1"),
                    )),
                ),
                (
                    "task-1-grok-reviewer",
                    Some(rev_approve(
                        "grok-review-1",
                        "impl-1",
                        Some(1),
                        Some("digest-1"),
                    )),
                ),
            ],
        ));

        assert!(eval.passed);
        assert_eq!(eval.reason, ExecutionGateReason::Passed);
    }

    #[test]
    fn task6_high_route_rejects_missing_second_reviewer_with_node_detail() {
        let eval = evaluate_execution_gate(&routed_task_input(
            impl_done("impl-1", 1, Some("digest-1")),
            vec![
                (
                    "task-1-codex-reviewer",
                    Some(rev_approve(
                        "codex-review-1",
                        "impl-1",
                        Some(1),
                        Some("digest-1"),
                    )),
                ),
                ("task-1-grok-reviewer", None),
            ],
        ));

        assert!(!eval.passed);
        assert_eq!(eval.reason, ExecutionGateReason::MissingReviewer);
        assert_eq!(
            eval.reviewer_node_id.as_deref(),
            Some("task-1-grok-reviewer")
        );
    }

    #[test]
    fn task6_each_non_passing_reviewer_state_fails_with_stable_reason() {
        for status in [
            TerminalRunStatus::Failed,
            TerminalRunStatus::Canceled,
            TerminalRunStatus::NonTerminal,
        ] {
            let mut second = rev_approve("grok-review-1", "impl-1", Some(1), Some("digest-1"));
            second.status = status;
            let eval = evaluate_execution_gate(&routed_task_input(
                impl_done("impl-1", 1, Some("digest-1")),
                vec![
                    (
                        "task-1-codex-reviewer",
                        Some(rev_approve(
                            "codex-review-1",
                            "impl-1",
                            Some(1),
                            Some("digest-1"),
                        )),
                    ),
                    ("task-1-grok-reviewer", Some(second)),
                ],
            ));
            assert!(!eval.passed, "status {status:?} must fail");
            assert_eq!(eval.reason, ExecutionGateReason::ReviewerNotTerminalPass);
            assert_eq!(
                eval.reviewer_node_id.as_deref(),
                Some("task-1-grok-reviewer")
            );
        }
    }

    #[test]
    fn task6_stale_empty_and_mismatched_second_reviewer_evidence_fails() {
        let cases = [
            (
                rev_approve("grok-review-old", "impl-old", Some(1), Some("digest-1")),
                ExecutionGateReason::ReviewerDoesNotCoverLatestImplementer,
            ),
            (
                rev_approve("grok-review-empty", "impl-1", Some(1), None),
                ExecutionGateReason::ArtifactDigestMismatch,
            ),
            (
                rev_approve("grok-review-wrong", "impl-1", Some(1), Some("digest-old")),
                ExecutionGateReason::ArtifactDigestMismatch,
            ),
        ];

        for (second, expected_reason) in cases {
            let eval = evaluate_execution_gate(&routed_task_input(
                impl_done("impl-1", 1, Some("digest-1")),
                vec![
                    (
                        "task-1-codex-reviewer",
                        Some(rev_approve(
                            "codex-review-1",
                            "impl-1",
                            Some(1),
                            Some("digest-1"),
                        )),
                    ),
                    ("task-1-grok-reviewer", Some(second)),
                ],
            ));
            assert!(!eval.passed);
            assert_eq!(eval.reason, expected_reason);
            assert_eq!(
                eval.reviewer_node_id.as_deref(),
                Some("task-1-grok-reviewer")
            );
        }
    }

    #[test]
    fn task6_one_approval_plus_one_request_changes_fails_strict_and() {
        let eval = evaluate_execution_gate(&routed_task_input(
            impl_done("impl-1", 1, Some("abc")),
            vec![
                (
                    "task-1-codex-reviewer",
                    Some(rev_approve(
                        "codex-review-1",
                        "impl-1",
                        Some(1),
                        Some("abc"),
                    )),
                ),
                (
                    "task-1-grok-reviewer",
                    Some(rev_changes("grok-review-1", "impl-1")),
                ),
            ],
        ));

        assert!(!eval.passed);
        assert_eq!(eval.reason, ExecutionGateReason::ReviewerNotTerminalPass);
        assert_eq!(
            eval.reviewer_node_id.as_deref(),
            Some("task-1-grok-reviewer")
        );
    }

    #[test]
    fn task6_new_producer_invalidates_every_prior_approval() {
        let prior = |task_id: &str| rev_approve(task_id, "impl-old", Some(1), Some("digest-old"));
        let eval = evaluate_execution_gate(&routed_task_input(
            impl_done("impl-new", 2, Some("digest-new")),
            vec![
                ("task-1-codex-reviewer", Some(prior("codex-review-old"))),
                ("task-1-grok-reviewer", Some(prior("grok-review-old"))),
            ],
        ));

        assert!(!eval.passed);
        assert_eq!(
            eval.reason,
            ExecutionGateReason::ReviewerDoesNotCoverLatestImplementer
        );
        assert_eq!(
            eval.reviewer_node_id.as_deref(),
            Some("task-1-codex-reviewer")
        );
    }

    #[test]
    fn task6_empty_producer_digest_fails_closed() {
        let eval = evaluate_execution_gate(&routed_task_input(
            impl_done("impl-1", 1, None),
            vec![(
                "task-1-codex-reviewer",
                Some(rev_approve("codex-review-1", "impl-1", Some(1), None)),
            )],
        ));

        assert!(!eval.passed);
        assert_eq!(eval.reason, ExecutionGateReason::ArtifactDigestMismatch);
        assert_eq!(eval.reviewer_node_id, None);
    }

    #[test]
    fn task6_new_final_fixer_invalidates_prior_reviewer_approval() {
        let eval = evaluate_execution_gate(&ExecutionGateInput {
            kind: ExecutionGateKind::Final,
            implementer_or_fixer: Some(impl_done("final-fixer-new", 2, Some("digest-new"))),
            required_reviewers: vec![RequiredReviewerEvidence {
                node_id: "final-reviewer".into(),
                evidence: Some(rev_approve(
                    "final-review-old",
                    "final-fixer-old",
                    Some(1),
                    Some("digest-old"),
                )),
            }],
            branch_tip_digest: None,
        });

        assert!(!eval.passed);
        assert_eq!(
            eval.reason,
            ExecutionGateReason::ReviewerDoesNotCoverLatestImplementer
        );
        assert_eq!(eval.reviewer_node_id.as_deref(), Some("final-reviewer"));
    }

    #[test]
    fn a7_task_pass_done_plus_approve() {
        let eval = evaluate_execution_gate(&task_input(
            Some(impl_done("impl-1", 1, Some("sha1"))),
            Some(rev_approve("rev-1", "impl-1", Some(1), Some("sha1"))),
        ));
        assert!(eval.passed);
        assert_eq!(eval.reason, ExecutionGateReason::Passed);
    }

    #[test]
    fn completion_v2_shared_validator_gate_ignores_legacy_card_projection() {
        let mut implementer = impl_done("impl-v2", 1, Some("sha-v2"));
        implementer.completion_protocol_version = 2;
        implementer.completion_state = Some(CompletionState::Resolved);
        implementer.completion_outcome = Some(CompletionOutcome::Done);
        implementer.completion_evidence_validated = true;
        implementer.summary_validated = false;
        implementer.work_status = Some(WorkStatus::Blocked);

        let mut reviewer = rev_approve("review-v2", "impl-v2", Some(1), Some("sha-v2"));
        reviewer.completion_protocol_version = 2;
        reviewer.completion_state = Some(CompletionState::Resolved);
        reviewer.completion_outcome = Some(CompletionOutcome::Approve);
        reviewer.completion_evidence_validated = true;
        reviewer.summary_validated = false;
        reviewer.review_verdict = Some(ReviewVerdict::Block);

        let evaluation = evaluate_execution_gate(&task_input(Some(implementer), Some(reviewer)));
        assert_eq!(evaluation.reason, ExecutionGateReason::Passed);
        assert!(evaluation.passed);
    }

    #[test]
    fn a7_task_pass_done_with_concerns_plus_approve_with_minors() {
        let eval = evaluate_execution_gate(&task_input(
            Some(impl_concerns("impl-1")),
            Some(rev_minors("rev-1", "impl-1", Some("abc"))),
        ));
        assert!(eval.passed);
    }

    #[test]
    fn a7_implementer_blocked_fails() {
        let eval = evaluate_execution_gate(&task_input(
            Some(impl_blocked("impl-1")),
            Some(rev_approve("rev-1", "impl-1", Some(1), Some("abc"))),
        ));
        assert!(!eval.passed);
        assert_eq!(eval.reason, ExecutionGateReason::ImplementerNotTerminalPass);
    }

    #[test]
    fn a7_implementer_needs_context_fails() {
        let eval = evaluate_execution_gate(&task_input(
            Some(impl_needs_context("impl-1")),
            Some(rev_approve("rev-1", "impl-1", Some(1), Some("abc"))),
        ));
        assert!(!eval.passed);
        assert_eq!(eval.reason, ExecutionGateReason::ImplementerNotTerminalPass);
    }

    #[test]
    fn a7_implementer_missing_summary_fails() {
        let mut impl_ev = impl_done("impl-1", 1, Some("sha1"));
        impl_ev.summary_validated = false;
        let eval = evaluate_execution_gate(&task_input(
            Some(impl_ev),
            Some(rev_approve("rev-1", "impl-1", Some(1), Some("sha1"))),
        ));
        assert!(!eval.passed);
        assert_eq!(eval.reason, ExecutionGateReason::ImplementerNotTerminalPass);
    }

    #[test]
    fn a7_implementer_failed_terminal_fails() {
        let mut impl_ev = impl_done("impl-1", 1, Some("sha1"));
        impl_ev.status = TerminalRunStatus::Failed;
        let eval = evaluate_execution_gate(&task_input(
            Some(impl_ev),
            Some(rev_approve("rev-1", "impl-1", Some(1), Some("sha1"))),
        ));
        assert!(!eval.passed);
    }

    #[test]
    fn a7_reviewer_request_changes_fails() {
        let eval = evaluate_execution_gate(&task_input(
            Some(impl_done("impl-1", 1, Some("sha1"))),
            Some(rev_changes("rev-1", "impl-1")),
        ));
        assert!(!eval.passed);
        assert_eq!(eval.reason, ExecutionGateReason::ReviewerNotTerminalPass);
    }

    #[test]
    fn a7_reviewer_block_fails() {
        let eval = evaluate_execution_gate(&task_input(
            Some(impl_done("impl-1", 1, Some("sha1"))),
            Some(rev_block("rev-1", "impl-1")),
        ));
        assert!(!eval.passed);
        assert_eq!(eval.reason, ExecutionGateReason::ReviewerNotTerminalPass);
    }

    #[test]
    fn a7_missing_implementer_fails() {
        let eval = evaluate_execution_gate(&task_input(
            None,
            Some(rev_approve("rev-1", "impl-1", Some(1), Some("sha1"))),
        ));
        assert!(!eval.passed);
        assert_eq!(eval.reason, ExecutionGateReason::MissingImplementer);
    }

    #[test]
    fn a7_missing_reviewer_fails() {
        let eval = evaluate_execution_gate(&task_input(
            Some(impl_done("impl-1", 1, Some("sha1"))),
            None,
        ));
        assert!(!eval.passed);
        assert_eq!(eval.reason, ExecutionGateReason::MissingReviewer);
    }

    // ---- B13 replacement-stale approval ----

    #[test]
    fn b13_replacement_stale_approval_rejected() {
        let latest = impl_done("impl-replacement", 1, Some("digest-collide"));
        let stale = rev_approve(
            "rev-1",
            "impl-pre-replacement",
            Some(5),
            Some("digest-collide"),
        );
        let eval = evaluate_execution_gate(&task_input(Some(latest), Some(stale)));
        assert!(!eval.passed);
        assert_eq!(
            eval.reason,
            ExecutionGateReason::ReviewerDoesNotCoverLatestImplementer
        );
    }

    #[test]
    fn b13_generation_informational_task_id_wins() {
        let impl_ev = impl_done("impl-1", 3, Some("sha1"));
        let mut rev = rev_approve("rev-1", "impl-1", Some(1), Some("sha1"));
        rev.reviewed_implementer_generation = Some(1);
        let eval = evaluate_execution_gate(&task_input(Some(impl_ev), Some(rev)));
        assert!(
            eval.passed,
            "generation is informational; task_id is authority"
        );
    }

    #[test]
    fn b13_empty_reviewed_task_id_rejected() {
        let mut rev = rev_approve("rev-1", "impl-1", Some(1), Some("sha1"));
        rev.reviewed_task_id = None;
        let eval = evaluate_execution_gate(&task_input(
            Some(impl_done("impl-1", 1, Some("sha1"))),
            Some(rev),
        ));
        assert!(!eval.passed);
        assert_eq!(
            eval.reason,
            ExecutionGateReason::ReviewerDoesNotCoverLatestImplementer
        );
    }

    // ---- B3 digest mismatch ----

    #[test]
    fn b3_digest_mismatch_rejected() {
        let eval = evaluate_execution_gate(&task_input(
            Some(impl_done("impl-1", 1, Some("digest-A"))),
            Some(rev_approve("rev-1", "impl-1", Some(1), Some("digest-B"))),
        ));
        assert!(!eval.passed);
        assert_eq!(eval.reason, ExecutionGateReason::ArtifactDigestMismatch);
    }

    #[test]
    fn task6_empty_producer_and_reviewer_digests_fail_closed() {
        let eval = evaluate_execution_gate(&task_input(
            Some(impl_done("impl-1", 1, None)),
            Some(rev_approve("rev-1", "impl-1", Some(1), None)),
        ));
        assert!(!eval.passed);
        assert_eq!(eval.reason, ExecutionGateReason::ArtifactDigestMismatch);
    }

    #[test]
    fn b3_implementer_digest_reviewer_missing_fails() {
        let eval = evaluate_execution_gate(&task_input(
            Some(impl_done("impl-1", 1, Some("digest-A"))),
            Some(rev_approve("rev-1", "impl-1", Some(1), None)),
        ));
        assert!(!eval.passed);
        assert_eq!(eval.reason, ExecutionGateReason::ArtifactDigestMismatch);
    }

    #[test]
    fn b3_reviewer_digest_implementer_absent_fails() {
        let eval = evaluate_execution_gate(&task_input(
            Some(impl_done("impl-1", 1, None)),
            Some(rev_approve(
                "rev-1",
                "impl-1",
                Some(1),
                Some("digest-only-on-rev"),
            )),
        ));
        assert!(!eval.passed);
        assert_eq!(eval.reason, ExecutionGateReason::ArtifactDigestMismatch);
    }

    #[test]
    fn final_first_pass_empty_task_id_fails() {
        let eval = evaluate_execution_gate(&final_input(
            None,
            Some(ExecutionGateRunEvidence {
                task_id: "".into(),
                generation: 1,
                status: TerminalRunStatus::Completed,
                completion_protocol_version: 1,
                completion_state: None,
                completion_outcome: None,
                completion_evidence_validated: false,
                summary_validated: true,
                work_status: None,
                review_verdict: Some(ReviewVerdict::Approve),
                artifact_digest: Some("tip".into()),
                reviewed_task_id: None,
                reviewed_implementer_generation: None,
            }),
            None,
        ));
        assert!(!eval.passed);
        assert_eq!(eval.reason, ExecutionGateReason::ReviewerNotTerminalPass);
    }

    #[test]
    fn final_first_pass_missing_summary_fails() {
        let eval = evaluate_execution_gate(&final_input(
            None,
            Some(ExecutionGateRunEvidence {
                task_id: "final-rev-1".into(),
                generation: 1,
                status: TerminalRunStatus::Completed,
                completion_protocol_version: 1,
                completion_state: None,
                completion_outcome: None,
                completion_evidence_validated: false,
                summary_validated: false,
                work_status: None,
                review_verdict: Some(ReviewVerdict::Approve),
                artifact_digest: Some("tip".into()),
                reviewed_task_id: None,
                reviewed_implementer_generation: None,
            }),
            None,
        ));
        assert!(!eval.passed);
    }

    #[test]
    fn final_first_pass_empty_digest_fails() {
        let eval = evaluate_execution_gate(&final_input(
            None,
            Some(ExecutionGateRunEvidence {
                task_id: "final-rev-1".into(),
                generation: 1,
                status: TerminalRunStatus::Completed,
                completion_protocol_version: 1,
                completion_state: None,
                completion_outcome: None,
                completion_evidence_validated: false,
                summary_validated: true,
                work_status: None,
                review_verdict: Some(ReviewVerdict::Approve),
                artifact_digest: None,
                reviewed_task_id: None,
                reviewed_implementer_generation: None,
            }),
            None,
        ));
        assert!(!eval.passed);
        assert_eq!(eval.reason, ExecutionGateReason::ArtifactDigestMismatch);
    }

    #[test]
    fn final_first_pass_branch_tip_mismatch_fails() {
        let eval = evaluate_execution_gate(&final_input(
            None,
            Some(ExecutionGateRunEvidence {
                task_id: "final-rev-1".into(),
                generation: 1,
                status: TerminalRunStatus::Completed,
                completion_protocol_version: 1,
                completion_state: None,
                completion_outcome: None,
                completion_evidence_validated: false,
                summary_validated: true,
                work_status: None,
                review_verdict: Some(ReviewVerdict::Approve),
                artifact_digest: Some("old-tip".into()),
                reviewed_task_id: None,
                reviewed_implementer_generation: None,
            }),
            Some("current-tip"),
        ));
        assert!(!eval.passed);
        assert_eq!(eval.reason, ExecutionGateReason::ArtifactDigestMismatch);
    }

    // ---- Final gate after fixer ----

    #[test]
    fn final_gate_after_fixer_pass() {
        let fixer = impl_done("fixer-1", 1, Some("tip"));
        let rev = rev_approve("final-rev-2", "fixer-1", Some(1), Some("tip"));
        let eval = evaluate_execution_gate(&final_input(Some(fixer), Some(rev), None));
        assert!(eval.passed);
    }

    #[test]
    fn final_gate_first_pass_reviewer_only() {
        let eval = evaluate_execution_gate(&final_input(
            None,
            Some(ExecutionGateRunEvidence {
                task_id: "final-rev-1".into(),
                generation: 1,
                status: TerminalRunStatus::Completed,
                completion_protocol_version: 1,
                completion_state: None,
                completion_outcome: None,
                completion_evidence_validated: false,
                summary_validated: true,
                work_status: None,
                review_verdict: Some(ReviewVerdict::Approve),
                artifact_digest: Some("branch-tip-sha".into()),
                reviewed_task_id: None,
                reviewed_implementer_generation: None,
            }),
            Some("branch-tip-sha"),
        ));
        assert!(eval.passed);
    }

    #[test]
    fn final_gate_fixer_present_stale_reviewer_rejected() {
        let fixer = impl_done("fixer-new", 1, Some("tip"));
        let stale = rev_approve("final-rev", "fixer-old", Some(1), Some("tip"));
        let eval = evaluate_execution_gate(&final_input(Some(fixer), Some(stale), None));
        assert!(!eval.passed);
        assert_eq!(
            eval.reason,
            ExecutionGateReason::ReviewerDoesNotCoverLatestImplementer
        );
    }

    #[test]
    fn final_gate_fixer_not_done_fails() {
        let mut fixer = impl_done("fixer-1", 1, Some("tip"));
        fixer.work_status = Some(WorkStatus::Blocked);
        let rev = rev_approve("final-rev", "fixer-1", Some(1), Some("tip"));
        let eval = evaluate_execution_gate(&final_input(Some(fixer), Some(rev), None));
        assert!(!eval.passed);
        assert_eq!(eval.reason, ExecutionGateReason::ImplementerNotTerminalPass);
    }
}
