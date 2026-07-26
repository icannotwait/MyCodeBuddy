//! Task / Final execution-gate evaluator (A7 + B3 + B13).
//!
//! Pure function over pre-loaded evidence. Document gates use
//! `settle_workflow_gate_core`; execution gates are projected only.

use crate::acp::delegation::card_summary::{ReviewVerdict, WorkStatus};

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

/// Input bundle for `evaluate_execution_gate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionGateInput {
    pub kind: ExecutionGateKind,
    /// Latest terminal Task implementer, or Final fixer when a fix cycle exists.
    pub implementer_or_fixer: Option<ExecutionGateRunEvidence>,
    /// Latest terminal Task/Final reviewer.
    pub reviewer: Option<ExecutionGateRunEvidence>,
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
}

/// Evaluate Task/Final execution-gate readiness (A7 + B3 + B13).
///
/// - Implementer/fixer terminal pass: completed + validated summary with
///   `done` / `done_with_concerns`.
/// - Reviewer terminal pass: completed + validated summary with
///   `approve` / `approve_with_minors`, **and** B13 exact `reviewed_task_id`
///   match against the latest implementer/fixer (when one exists), **and**
///   B3 digest match when both digests are present.
/// - `reviewed_implementer_generation` is informational only.
/// - Final first-pass (no fixer terminal): reviewer approve alone may pass.
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
    let Some(rev) = input.reviewer.as_ref() else {
        return fail(ExecutionGateReason::MissingReviewer);
    };
    if !reviewer_verdict_pass(rev) {
        return fail(ExecutionGateReason::ReviewerNotTerminalPass);
    }
    if let Err(reason) = reviewer_covers_implementer(rev, impl_ev) {
        return fail(reason);
    }
    pass()
}

fn evaluate_final_gate(input: &ExecutionGateInput) -> ExecutionGateEval {
    // When a Final fixer terminal exists, gate requires fixer pass + reviewer
    // covering that exact fixer run (same as Task pair).
    if let Some(fixer) = input.implementer_or_fixer.as_ref() {
        if !implementer_terminal_pass(fixer) {
            return fail(ExecutionGateReason::ImplementerNotTerminalPass);
        }
        let Some(rev) = input.reviewer.as_ref() else {
            return fail(ExecutionGateReason::MissingReviewer);
        };
        if !reviewer_verdict_pass(rev) {
            return fail(ExecutionGateReason::ReviewerNotTerminalPass);
        }
        if let Err(reason) = reviewer_covers_implementer(rev, fixer) {
            return fail(reason);
        }
        return pass();
    }

    // First-pass Final: no fixer terminal — reviewer approve alone.
    let Some(rev) = input.reviewer.as_ref() else {
        return fail(ExecutionGateReason::MissingReviewer);
    };
    if !reviewer_verdict_pass(rev) {
        return fail(ExecutionGateReason::ReviewerNotTerminalPass);
    }
    pass()
}

fn implementer_terminal_pass(ev: &ExecutionGateRunEvidence) -> bool {
    if !matches!(ev.status, TerminalRunStatus::Completed) {
        return false;
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
    if !ev.summary_validated {
        return false;
    }
    matches!(
        ev.review_verdict,
        Some(ReviewVerdict::Approve) | Some(ReviewVerdict::ApproveWithMinors)
    )
}

/// B13 exact task_id coverage + B3 digest match when present.
fn reviewer_covers_implementer(
    rev: &ExecutionGateRunEvidence,
    impl_ev: &ExecutionGateRunEvidence,
) -> Result<(), ExecutionGateReason> {
    // B13: generation is informational; task_id is authoritative.
    match rev.reviewed_task_id.as_deref() {
        Some(id) if id == impl_ev.task_id => {}
        _ => return Err(ExecutionGateReason::ReviewerDoesNotCoverLatestImplementer),
    }

    // B3: when both digests are present they must match. Empty/None skips.
    match (
        rev.artifact_digest.as_deref(),
        impl_ev.artifact_digest.as_deref(),
    ) {
        (Some(a), Some(b)) if !a.is_empty() && !b.is_empty() && a != b => {
            return Err(ExecutionGateReason::ArtifactDigestMismatch);
        }
        _ => {}
    }

    // Still require B13 even when digests match (B13 second clause).
    // Already enforced above.

    let _ = rev.reviewed_implementer_generation; // informational only
    Ok(())
}

fn pass() -> ExecutionGateEval {
    ExecutionGateEval {
        passed: true,
        reason: ExecutionGateReason::Passed,
    }
}

fn fail(reason: ExecutionGateReason) -> ExecutionGateEval {
    ExecutionGateEval {
        passed: false,
        reason,
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

    #[test]
    fn a7_task_pass_done_plus_approve() {
        let eval = evaluate_execution_gate(&ExecutionGateInput {
            kind: ExecutionGateKind::Task,
            implementer_or_fixer: Some(impl_done("impl-1", 1, Some("sha1"))),
            reviewer: Some(rev_approve("rev-1", "impl-1", Some(1), Some("sha1"))),
        });
        assert!(eval.passed);
        assert_eq!(eval.reason, ExecutionGateReason::Passed);
    }

    #[test]
    fn a7_task_pass_done_with_concerns_plus_approve_with_minors() {
        let eval = evaluate_execution_gate(&ExecutionGateInput {
            kind: ExecutionGateKind::Task,
            implementer_or_fixer: Some(impl_concerns("impl-1")),
            reviewer: Some(rev_minors("rev-1", "impl-1", Some("abc"))),
        });
        assert!(eval.passed);
    }

    #[test]
    fn a7_implementer_blocked_fails() {
        let eval = evaluate_execution_gate(&ExecutionGateInput {
            kind: ExecutionGateKind::Task,
            implementer_or_fixer: Some(impl_blocked("impl-1")),
            reviewer: Some(rev_approve("rev-1", "impl-1", Some(1), Some("abc"))),
        });
        assert!(!eval.passed);
        assert_eq!(
            eval.reason,
            ExecutionGateReason::ImplementerNotTerminalPass
        );
    }

    #[test]
    fn a7_implementer_needs_context_fails() {
        let eval = evaluate_execution_gate(&ExecutionGateInput {
            kind: ExecutionGateKind::Task,
            implementer_or_fixer: Some(impl_needs_context("impl-1")),
            reviewer: Some(rev_approve("rev-1", "impl-1", Some(1), Some("abc"))),
        });
        assert!(!eval.passed);
        assert_eq!(
            eval.reason,
            ExecutionGateReason::ImplementerNotTerminalPass
        );
    }

    #[test]
    fn a7_implementer_missing_summary_fails() {
        let mut impl_ev = impl_done("impl-1", 1, Some("sha1"));
        impl_ev.summary_validated = false;
        let eval = evaluate_execution_gate(&ExecutionGateInput {
            kind: ExecutionGateKind::Task,
            implementer_or_fixer: Some(impl_ev),
            reviewer: Some(rev_approve("rev-1", "impl-1", Some(1), Some("sha1"))),
        });
        assert!(!eval.passed);
        assert_eq!(
            eval.reason,
            ExecutionGateReason::ImplementerNotTerminalPass
        );
    }

    #[test]
    fn a7_implementer_failed_terminal_fails() {
        let mut impl_ev = impl_done("impl-1", 1, Some("sha1"));
        impl_ev.status = TerminalRunStatus::Failed;
        let eval = evaluate_execution_gate(&ExecutionGateInput {
            kind: ExecutionGateKind::Task,
            implementer_or_fixer: Some(impl_ev),
            reviewer: Some(rev_approve("rev-1", "impl-1", Some(1), Some("sha1"))),
        });
        assert!(!eval.passed);
    }

    #[test]
    fn a7_reviewer_request_changes_fails() {
        let eval = evaluate_execution_gate(&ExecutionGateInput {
            kind: ExecutionGateKind::Task,
            implementer_or_fixer: Some(impl_done("impl-1", 1, Some("sha1"))),
            reviewer: Some(rev_changes("rev-1", "impl-1")),
        });
        assert!(!eval.passed);
        assert_eq!(eval.reason, ExecutionGateReason::ReviewerNotTerminalPass);
    }

    #[test]
    fn a7_reviewer_block_fails() {
        let eval = evaluate_execution_gate(&ExecutionGateInput {
            kind: ExecutionGateKind::Task,
            implementer_or_fixer: Some(impl_done("impl-1", 1, Some("sha1"))),
            reviewer: Some(rev_block("rev-1", "impl-1")),
        });
        assert!(!eval.passed);
        assert_eq!(eval.reason, ExecutionGateReason::ReviewerNotTerminalPass);
    }

    #[test]
    fn a7_missing_implementer_fails() {
        let eval = evaluate_execution_gate(&ExecutionGateInput {
            kind: ExecutionGateKind::Task,
            implementer_or_fixer: None,
            reviewer: Some(rev_approve("rev-1", "impl-1", Some(1), Some("sha1"))),
        });
        assert!(!eval.passed);
        assert_eq!(eval.reason, ExecutionGateReason::MissingImplementer);
    }

    #[test]
    fn a7_missing_reviewer_fails() {
        let eval = evaluate_execution_gate(&ExecutionGateInput {
            kind: ExecutionGateKind::Task,
            implementer_or_fixer: Some(impl_done("impl-1", 1, Some("sha1"))),
            reviewer: None,
        });
        assert!(!eval.passed);
        assert_eq!(eval.reason, ExecutionGateReason::MissingReviewer);
    }

    // ---- B13 replacement-stale approval ----

    #[test]
    fn b13_replacement_stale_approval_rejected() {
        // Latest implementer is a replacement child (new task_id).
        let latest = impl_done("impl-replacement", 1, Some("digest-collide"));
        // Reviewer still points at the pre-replacement child.
        let stale = rev_approve(
            "rev-1",
            "impl-pre-replacement",
            Some(5),
            Some("digest-collide"),
        );
        let eval = evaluate_execution_gate(&ExecutionGateInput {
            kind: ExecutionGateKind::Task,
            implementer_or_fixer: Some(latest),
            reviewer: Some(stale),
        });
        assert!(!eval.passed);
        assert_eq!(
            eval.reason,
            ExecutionGateReason::ReviewerDoesNotCoverLatestImplementer
        );
    }

    #[test]
    fn b13_generation_informational_task_id_wins() {
        // Generation on binding is stale/wrong but task_id is exact → pass.
        let impl_ev = impl_done("impl-1", 3, Some("sha1"));
        let mut rev = rev_approve("rev-1", "impl-1", Some(1), Some("sha1"));
        rev.reviewed_implementer_generation = Some(1); // older than 3
        let eval = evaluate_execution_gate(&ExecutionGateInput {
            kind: ExecutionGateKind::Task,
            implementer_or_fixer: Some(impl_ev),
            reviewer: Some(rev),
        });
        assert!(eval.passed, "generation is informational; task_id is authority");
    }

    #[test]
    fn b13_empty_reviewed_task_id_rejected() {
        let mut rev = rev_approve("rev-1", "impl-1", Some(1), Some("sha1"));
        rev.reviewed_task_id = None;
        let eval = evaluate_execution_gate(&ExecutionGateInput {
            kind: ExecutionGateKind::Task,
            implementer_or_fixer: Some(impl_done("impl-1", 1, Some("sha1"))),
            reviewer: Some(rev),
        });
        assert!(!eval.passed);
        assert_eq!(
            eval.reason,
            ExecutionGateReason::ReviewerDoesNotCoverLatestImplementer
        );
    }

    // ---- B3 digest mismatch ----

    #[test]
    fn b3_digest_mismatch_rejected() {
        let eval = evaluate_execution_gate(&ExecutionGateInput {
            kind: ExecutionGateKind::Task,
            implementer_or_fixer: Some(impl_done("impl-1", 1, Some("digest-A"))),
            reviewer: Some(rev_approve("rev-1", "impl-1", Some(1), Some("digest-B"))),
        });
        assert!(!eval.passed);
        assert_eq!(eval.reason, ExecutionGateReason::ArtifactDigestMismatch);
    }

    #[test]
    fn b3_empty_digests_rely_on_task_id() {
        let eval = evaluate_execution_gate(&ExecutionGateInput {
            kind: ExecutionGateKind::Task,
            implementer_or_fixer: Some(impl_done("impl-1", 1, None)),
            reviewer: Some(rev_approve("rev-1", "impl-1", Some(1), None)),
        });
        assert!(eval.passed);
    }

    // ---- Final gate after fixer ----

    #[test]
    fn final_gate_after_fixer_pass() {
        let fixer = impl_done("fixer-1", 1, Some("tip"));
        let rev = rev_approve("final-rev-2", "fixer-1", Some(1), Some("tip"));
        let eval = evaluate_execution_gate(&ExecutionGateInput {
            kind: ExecutionGateKind::Final,
            implementer_or_fixer: Some(fixer),
            reviewer: Some(rev),
        });
        assert!(eval.passed);
    }

    #[test]
    fn final_gate_first_pass_reviewer_only() {
        let eval = evaluate_execution_gate(&ExecutionGateInput {
            kind: ExecutionGateKind::Final,
            implementer_or_fixer: None,
            reviewer: Some(ExecutionGateRunEvidence {
                task_id: "final-rev-1".into(),
                generation: 1,
                status: TerminalRunStatus::Completed,
                summary_validated: true,
                work_status: None,
                review_verdict: Some(ReviewVerdict::Approve),
                artifact_digest: None,
                reviewed_task_id: None,
                reviewed_implementer_generation: None,
            }),
        });
        assert!(eval.passed);
    }

    #[test]
    fn final_gate_fixer_present_stale_reviewer_rejected() {
        let fixer = impl_done("fixer-new", 1, Some("tip"));
        let stale = rev_approve("final-rev", "fixer-old", Some(1), Some("tip"));
        let eval = evaluate_execution_gate(&ExecutionGateInput {
            kind: ExecutionGateKind::Final,
            implementer_or_fixer: Some(fixer),
            reviewer: Some(stale),
        });
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
        let eval = evaluate_execution_gate(&ExecutionGateInput {
            kind: ExecutionGateKind::Final,
            implementer_or_fixer: Some(fixer),
            reviewer: Some(rev),
        });
        assert!(!eval.passed);
        assert_eq!(
            eval.reason,
            ExecutionGateReason::ImplementerNotTerminalPass
        );
    }
}
