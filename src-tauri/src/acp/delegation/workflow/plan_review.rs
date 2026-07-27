//! Pure derivation for adaptive Plan review rounds.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_ID_CHARS: usize = 200;
const MAX_TEXT_BYTES: usize = 4 * 1024;
const MAX_OWNER_COUNT: usize = 64;
const MAX_FINDING_COUNT: usize = 400;
const MAX_ROUND_JSON_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanReviewScope {
    Full,
    Scoped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanRevisionKind {
    Initial,
    Localized,
    Material,
    HolisticRewrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingSeverity {
    Critical,
    Important,
    Minor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingStatus {
    Open,
    Resolved,
    New,
    Reopened,
}

impl FindingStatus {
    fn is_open(self) -> bool {
        !matches!(self, Self::Resolved)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanFindingUpdate {
    pub finding_id: String,
    pub severity: FindingSeverity,
    pub status: FindingStatus,
    pub owner_reviewer_node_ids: Vec<String>,
    pub summary: String,
    pub evidence_ref: String,
    pub report_file: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanReviewRoundSubmission {
    pub scope: PlanReviewScope,
    pub revision_kind: PlanRevisionKind,
    pub scope_reason: String,
    pub covered_author_task_id: String,
    pub covered_plan_digest: String,
    pub required_reviewer_node_ids: Vec<String>,
    pub finding_updates: Vec<PlanFindingUpdate>,
    pub lineage_reset_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanReviewNextAction {
    ContinueReview,
    HolisticRewriteRequired,
    UserDecisionRequired,
    Approved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanReviewRoundState {
    pub scope: PlanReviewScope,
    pub revision_kind: PlanRevisionKind,
    pub scope_reason: String,
    pub covered_author_task_id: String,
    pub covered_plan_digest: String,
    /// Reviewers whose completed evidence formed this round.
    pub reviewed_reviewer_node_ids: Vec<String>,
    /// Reviewers required for the next round, if another round is needed.
    pub next_required_reviewer_node_ids: Vec<String>,
    pub findings: Vec<PlanFindingUpdate>,
    pub lineage_reset_reason: Option<String>,
    pub critical_count: u32,
    pub important_count: u32,
    pub minor_count: u32,
    pub net_improvement: bool,
    pub stagnation_count: u32,
    pub rewrite_used: bool,
    pub next_action: PlanReviewNextAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PlanReviewError {
    #[error("invalid Plan review field: {0}")]
    InvalidField(String),
    #[error("Plan review bound exceeded: {0}")]
    BoundsExceeded(String),
    #[error("unknown Plan reviewer node id: {0}")]
    UnknownReviewerNodeId(String),
    #[error("finding {finding_id} cannot change severity")]
    SeverityMutation { finding_id: String },
    #[error("required reviewer set mismatch: expected {expected:?}, got {actual:?}")]
    RequiredReviewerSetMismatch {
        expected: Vec<String>,
        actual: Vec<String>,
    },
    #[error("requirements lineage reset requires a non-empty bounded reason")]
    InvalidLineageResetReason,
    #[error("invalid Plan review transition: {0}")]
    InvalidTransition(String),
}

/// Derives state for one completed round. Incomplete or infrastructure-failed
/// rounds must not call this function and therefore cannot advance stagnation.
pub fn derive_plan_review_round(
    prior: Option<&PlanReviewRoundState>,
    reviewer_cohort_node_ids: &[String],
    submission: &PlanReviewRoundSubmission,
) -> Result<PlanReviewRoundState, PlanReviewError> {
    validate_submission_size(submission)?;
    validate_text("scope_reason", &submission.scope_reason)?;
    validate_id("covered_author_task_id", &submission.covered_author_task_id)?;
    validate_id("covered_plan_digest", &submission.covered_plan_digest)?;

    let cohort = canonical_reviewer_set("reviewer_cohort_node_ids", reviewer_cohort_node_ids)?;
    if cohort.is_empty() {
        return Err(PlanReviewError::InvalidField(
            "reviewer_cohort_node_ids must not be empty".to_owned(),
        ));
    }
    let reviewed = canonical_reviewer_set(
        "required_reviewer_node_ids",
        &submission.required_reviewer_node_ids,
    )?;
    validate_known_reviewers(&cohort, &reviewed)?;

    let lineage_reset = validate_lineage_reset(prior, submission)?;
    validate_transition(prior, submission, lineage_reset)?;
    validate_review_scope(prior, &cohort, &reviewed, submission, lineage_reset)?;

    if submission.finding_updates.len() > MAX_FINDING_COUNT {
        return Err(PlanReviewError::BoundsExceeded(format!(
            "finding_updates count exceeds {MAX_FINDING_COUNT}"
        )));
    }

    let mut findings: BTreeMap<String, PlanFindingUpdate> = if lineage_reset {
        BTreeMap::new()
    } else {
        prior
            .map(|state| {
                state
                    .findings
                    .iter()
                    .cloned()
                    .map(|finding| (finding.finding_id.clone(), finding))
                    .collect()
            })
            .unwrap_or_default()
    };

    let mut updated_this_round = BTreeSet::new();
    for update in &submission.finding_updates {
        validate_finding(update, &cohort, &reviewed)?;
        let mut update = update.clone();
        update.owner_reviewer_node_ids = canonical_owners(&update.owner_reviewer_node_ids)?;
        let duplicate_update = !updated_this_round.insert(update.finding_id.clone());

        if let Some(existing) = findings.get_mut(&update.finding_id) {
            if existing.severity != update.severity {
                return Err(PlanReviewError::SeverityMutation {
                    finding_id: update.finding_id,
                });
            }

            let mut owners: BTreeSet<String> =
                existing.owner_reviewer_node_ids.iter().cloned().collect();
            owners.extend(update.owner_reviewer_node_ids);
            if owners.len() > MAX_OWNER_COUNT {
                return Err(PlanReviewError::BoundsExceeded(format!(
                    "finding {} owner count exceeds {MAX_OWNER_COUNT}",
                    update.finding_id
                )));
            }
            update.owner_reviewer_node_ids = owners.into_iter().collect();
            if duplicate_update {
                update.status = merge_same_round_status(existing.status, update.status);
            }
            *existing = update;
        } else {
            findings.insert(update.finding_id.clone(), update);
        }
    }

    if findings.len() > MAX_FINDING_COUNT {
        return Err(PlanReviewError::BoundsExceeded(format!(
            "finding ledger count exceeds {MAX_FINDING_COUNT}"
        )));
    }

    for finding in findings.values() {
        validate_known_reviewers(&cohort, &finding.owner_reviewer_node_ids)?;
    }

    for finding in findings.values().filter(|finding| {
        finding.status == FindingStatus::Resolved
            && matches!(
                finding.severity,
                FindingSeverity::Critical | FindingSeverity::Important
            )
    }) {
        let all_owners_reviewed = finding
            .owner_reviewer_node_ids
            .iter()
            .all(|owner| reviewed.binary_search(owner).is_ok());
        if !all_owners_reviewed
            && submission.finding_updates.iter().any(|update| {
                update.finding_id == finding.finding_id && update.status == FindingStatus::Resolved
            })
        {
            return Err(PlanReviewError::InvalidTransition(format!(
                "finding {} cannot resolve until every owner completes the round",
                finding.finding_id
            )));
        }
    }

    let findings: Vec<PlanFindingUpdate> = findings.into_values().collect();
    let (critical_count, important_count, minor_count) = count_open_findings(&findings);
    let is_baseline = prior.is_none() || lineage_reset;
    let net_improvement = prior.is_some_and(|state| {
        !lineage_reset
            && critical_count <= state.critical_count
            && critical_count + important_count < state.critical_count + state.important_count
    });

    let (rewrite_used, base_stagnation) = if is_baseline {
        (false, 0)
    } else if submission.revision_kind == PlanRevisionKind::HolisticRewrite {
        (true, 0)
    } else {
        let state = prior.expect("non-baseline derivation has prior state");
        (state.rewrite_used, state.stagnation_count)
    };
    let stagnation_count = if is_baseline || net_improvement {
        0
    } else {
        base_stagnation.saturating_add(1)
    };

    let next_action = if critical_count == 0 && important_count == 0 {
        PlanReviewNextAction::Approved
    } else if stagnation_count >= 2 && rewrite_used {
        PlanReviewNextAction::UserDecisionRequired
    } else if stagnation_count >= 2 {
        PlanReviewNextAction::HolisticRewriteRequired
    } else {
        PlanReviewNextAction::ContinueReview
    };

    let next_required_reviewer_node_ids = match next_action {
        PlanReviewNextAction::Approved => Vec::new(),
        PlanReviewNextAction::HolisticRewriteRequired => cohort.clone(),
        PlanReviewNextAction::ContinueReview | PlanReviewNextAction::UserDecisionRequired => {
            blocking_finding_owners(&findings)
        }
    };

    let state = PlanReviewRoundState {
        scope: submission.scope,
        revision_kind: submission.revision_kind,
        scope_reason: submission.scope_reason.clone(),
        covered_author_task_id: submission.covered_author_task_id.clone(),
        covered_plan_digest: submission.covered_plan_digest.clone(),
        reviewed_reviewer_node_ids: reviewed,
        next_required_reviewer_node_ids,
        findings,
        lineage_reset_reason: submission.lineage_reset_reason.clone(),
        critical_count,
        important_count,
        minor_count,
        net_improvement,
        stagnation_count,
        rewrite_used,
        next_action,
    };
    validate_state_size(&state)?;
    Ok(state)
}

fn merge_same_round_status(left: FindingStatus, right: FindingStatus) -> FindingStatus {
    if left == FindingStatus::Reopened || right == FindingStatus::Reopened {
        FindingStatus::Reopened
    } else if left == FindingStatus::New || right == FindingStatus::New {
        FindingStatus::New
    } else if left == FindingStatus::Open || right == FindingStatus::Open {
        FindingStatus::Open
    } else {
        FindingStatus::Resolved
    }
}

fn validate_submission_size(submission: &PlanReviewRoundSubmission) -> Result<(), PlanReviewError> {
    let size = serde_json::to_vec(submission)
        .map_err(|error| PlanReviewError::InvalidField(error.to_string()))?
        .len();
    if size > MAX_ROUND_JSON_BYTES {
        return Err(PlanReviewError::BoundsExceeded(format!(
            "submission JSON is {size} bytes, maximum is {MAX_ROUND_JSON_BYTES}"
        )));
    }
    Ok(())
}

fn validate_state_size(state: &PlanReviewRoundState) -> Result<(), PlanReviewError> {
    let size = serde_json::to_vec(state)
        .map_err(|error| PlanReviewError::InvalidField(error.to_string()))?
        .len();
    if size > MAX_ROUND_JSON_BYTES {
        return Err(PlanReviewError::BoundsExceeded(format!(
            "derived state JSON is {size} bytes, maximum is {MAX_ROUND_JSON_BYTES}"
        )));
    }
    Ok(())
}

fn validate_lineage_reset(
    prior: Option<&PlanReviewRoundState>,
    submission: &PlanReviewRoundSubmission,
) -> Result<bool, PlanReviewError> {
    let Some(reason) = submission.lineage_reset_reason.as_deref() else {
        return Ok(false);
    };
    if prior.is_none() || reason.trim().is_empty() || reason.len() > MAX_TEXT_BYTES {
        return Err(PlanReviewError::InvalidLineageResetReason);
    }
    Ok(true)
}

fn validate_transition(
    prior: Option<&PlanReviewRoundState>,
    submission: &PlanReviewRoundSubmission,
    lineage_reset: bool,
) -> Result<(), PlanReviewError> {
    match prior {
        None => {
            if submission.revision_kind != PlanRevisionKind::Initial {
                return Err(PlanReviewError::InvalidTransition(
                    "the first completed round must be initial".to_owned(),
                ));
            }
        }
        Some(_) if lineage_reset => {
            if submission.revision_kind != PlanRevisionKind::Initial {
                return Err(PlanReviewError::InvalidTransition(
                    "a requirements lineage reset must establish an initial round".to_owned(),
                ));
            }
        }
        Some(state) => {
            if submission.revision_kind == PlanRevisionKind::Initial {
                return Err(PlanReviewError::InvalidTransition(
                    "initial is only valid for a new requirements lineage".to_owned(),
                ));
            }
            if state.next_action == PlanReviewNextAction::UserDecisionRequired {
                return Err(PlanReviewError::InvalidTransition(
                    "user decision is required before another completed round".to_owned(),
                ));
            }
            if state.next_action == PlanReviewNextAction::HolisticRewriteRequired
                && submission.revision_kind != PlanRevisionKind::HolisticRewrite
            {
                return Err(PlanReviewError::InvalidTransition(
                    "the next completed round must cover the required holistic rewrite".to_owned(),
                ));
            }
            if submission.revision_kind == PlanRevisionKind::HolisticRewrite
                && (state.next_action != PlanReviewNextAction::HolisticRewriteRequired
                    || state.rewrite_used)
            {
                return Err(PlanReviewError::InvalidTransition(
                    "holistic rewrite is not currently authorized".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

fn validate_review_scope(
    prior: Option<&PlanReviewRoundState>,
    cohort: &[String],
    reviewed: &[String],
    submission: &PlanReviewRoundSubmission,
    lineage_reset: bool,
) -> Result<(), PlanReviewError> {
    let expected = match submission.scope {
        PlanReviewScope::Full => cohort,
        PlanReviewScope::Scoped => {
            if submission.revision_kind != PlanRevisionKind::Localized || lineage_reset {
                return Err(PlanReviewError::InvalidTransition(
                    "only a localized revision in the current lineage may use scoped review"
                        .to_owned(),
                ));
            }
            prior
                .map(|state| state.next_required_reviewer_node_ids.as_slice())
                .ok_or_else(|| {
                    PlanReviewError::InvalidTransition(
                        "the first completed round must use full review".to_owned(),
                    )
                })?
        }
    };
    if expected != reviewed {
        return Err(PlanReviewError::RequiredReviewerSetMismatch {
            expected: expected.to_vec(),
            actual: reviewed.to_vec(),
        });
    }
    Ok(())
}

fn validate_finding(
    finding: &PlanFindingUpdate,
    cohort: &[String],
    reviewed: &[String],
) -> Result<(), PlanReviewError> {
    validate_id("finding_id", &finding.finding_id)?;
    validate_text("summary", &finding.summary)?;
    validate_text("evidence_ref", &finding.evidence_ref)?;
    validate_text("report_file", &finding.report_file)?;
    let owners = canonical_owners(&finding.owner_reviewer_node_ids)?;
    validate_known_reviewers(cohort, &owners)?;
    for owner in owners {
        if reviewed.binary_search(&owner).is_err() {
            return Err(PlanReviewError::InvalidField(format!(
                "finding owner {owner} did not complete this round"
            )));
        }
    }
    Ok(())
}

fn validate_known_reviewers(
    cohort: &[String],
    reviewers: &[String],
) -> Result<(), PlanReviewError> {
    for reviewer in reviewers {
        if cohort.binary_search(reviewer).is_err() {
            return Err(PlanReviewError::UnknownReviewerNodeId(reviewer.clone()));
        }
    }
    Ok(())
}

fn canonical_reviewer_set(
    field: &str,
    reviewers: &[String],
) -> Result<Vec<String>, PlanReviewError> {
    if reviewers.len() > MAX_OWNER_COUNT {
        return Err(PlanReviewError::BoundsExceeded(format!(
            "{field} count exceeds {MAX_OWNER_COUNT}"
        )));
    }
    let mut result = BTreeSet::new();
    for reviewer in reviewers {
        validate_id(field, reviewer)?;
        if !result.insert(reviewer.clone()) {
            return Err(PlanReviewError::InvalidField(format!(
                "{field} contains duplicate reviewer {reviewer}"
            )));
        }
    }
    Ok(result.into_iter().collect())
}

fn canonical_owners(owners: &[String]) -> Result<Vec<String>, PlanReviewError> {
    if owners.is_empty() {
        return Err(PlanReviewError::InvalidField(
            "owner_reviewer_node_ids must not be empty".to_owned(),
        ));
    }
    let owners: BTreeSet<String> = owners.iter().cloned().collect();
    if owners.len() > MAX_OWNER_COUNT {
        return Err(PlanReviewError::BoundsExceeded(format!(
            "finding owner count exceeds {MAX_OWNER_COUNT}"
        )));
    }
    Ok(owners.into_iter().collect())
}

fn validate_id(field: &str, value: &str) -> Result<(), PlanReviewError> {
    if value.trim().is_empty() {
        return Err(PlanReviewError::InvalidField(format!(
            "{field} must not be empty"
        )));
    }
    if value.chars().count() > MAX_ID_CHARS {
        return Err(PlanReviewError::BoundsExceeded(format!(
            "{field} exceeds {MAX_ID_CHARS} characters"
        )));
    }
    Ok(())
}

fn validate_text(field: &str, value: &str) -> Result<(), PlanReviewError> {
    if value.trim().is_empty() {
        return Err(PlanReviewError::InvalidField(format!(
            "{field} must not be empty"
        )));
    }
    if value.len() > MAX_TEXT_BYTES {
        return Err(PlanReviewError::BoundsExceeded(format!(
            "{field} exceeds {MAX_TEXT_BYTES} bytes"
        )));
    }
    Ok(())
}

fn count_open_findings(findings: &[PlanFindingUpdate]) -> (u32, u32, u32) {
    let mut counts = (0, 0, 0);
    for finding in findings.iter().filter(|finding| finding.status.is_open()) {
        match finding.severity {
            FindingSeverity::Critical => counts.0 += 1,
            FindingSeverity::Important => counts.1 += 1,
            FindingSeverity::Minor => counts.2 += 1,
        }
    }
    counts
}

fn blocking_finding_owners(findings: &[PlanFindingUpdate]) -> Vec<String> {
    findings
        .iter()
        .filter(|finding| {
            finding.status.is_open()
                && matches!(
                    finding.severity,
                    FindingSeverity::Critical | FindingSeverity::Important
                )
        })
        .flat_map(|finding| finding.owner_reviewer_node_ids.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn finding(
        finding_id: &str,
        severity: FindingSeverity,
        status: FindingStatus,
        owners: &[&str],
    ) -> PlanFindingUpdate {
        PlanFindingUpdate {
            finding_id: finding_id.to_owned(),
            severity,
            status,
            owner_reviewer_node_ids: ids(owners),
            summary: format!("summary for {finding_id}"),
            evidence_ref: format!("evidence/{finding_id}"),
            report_file: format!("reports/{finding_id}.md"),
        }
    }

    fn submission(
        scope: PlanReviewScope,
        revision_kind: PlanRevisionKind,
        required: &[&str],
        findings: Vec<PlanFindingUpdate>,
    ) -> PlanReviewRoundSubmission {
        PlanReviewRoundSubmission {
            scope,
            revision_kind,
            scope_reason: "review the current author artifact".to_owned(),
            covered_author_task_id: "author-task-1".to_owned(),
            covered_plan_digest: "sha256:plan-1".to_owned(),
            required_reviewer_node_ids: ids(required),
            finding_updates: findings,
            lineage_reset_reason: None,
        }
    }

    fn initial(findings: Vec<PlanFindingUpdate>) -> PlanReviewRoundState {
        derive_plan_review_round(
            None,
            &ids(&["reviewer-a", "reviewer-b", "reviewer-c"]),
            &submission(
                PlanReviewScope::Full,
                PlanRevisionKind::Initial,
                &["reviewer-a", "reviewer-b", "reviewer-c"],
                findings,
            ),
        )
        .expect("initial full round should be valid")
    }

    fn next(
        prior: &PlanReviewRoundState,
        revision_kind: PlanRevisionKind,
        findings: Vec<PlanFindingUpdate>,
    ) -> PlanReviewRoundState {
        let required: Vec<&str> = prior
            .next_required_reviewer_node_ids
            .iter()
            .map(String::as_str)
            .collect();
        derive_plan_review_round(
            Some(prior),
            &ids(&["reviewer-a", "reviewer-b", "reviewer-c"]),
            &submission(
                if revision_kind == PlanRevisionKind::Localized {
                    PlanReviewScope::Scoped
                } else {
                    PlanReviewScope::Full
                },
                revision_kind,
                &required,
                findings,
            ),
        )
        .expect("next round should be valid")
    }

    #[test]
    fn stable_id_reuse_updates_finding_without_duplicating_it() {
        let prior = initial(vec![finding(
            "F-1",
            FindingSeverity::Important,
            FindingStatus::New,
            &["reviewer-a"],
        )]);
        let state = next(
            &prior,
            PlanRevisionKind::Localized,
            vec![finding(
                "F-1",
                FindingSeverity::Important,
                FindingStatus::Open,
                &["reviewer-a"],
            )],
        );

        assert_eq!(state.findings.len(), 1);
        assert_eq!(state.findings[0].finding_id, "F-1");
        assert_eq!(state.findings[0].status, FindingStatus::Open);
    }

    #[test]
    fn duplicate_owner_union_is_sorted_and_any_reopen_wins() {
        let prior = initial(vec![finding(
            "F-1",
            FindingSeverity::Critical,
            FindingStatus::Open,
            &["reviewer-a", "reviewer-a"],
        )]);
        let state = derive_plan_review_round(
            Some(&prior),
            &ids(&["reviewer-a", "reviewer-b", "reviewer-c"]),
            &submission(
                PlanReviewScope::Full,
                PlanRevisionKind::Localized,
                &["reviewer-a", "reviewer-b", "reviewer-c"],
                vec![
                    finding(
                        "F-1",
                        FindingSeverity::Critical,
                        FindingStatus::Reopened,
                        &["reviewer-b"],
                    ),
                    finding(
                        "F-1",
                        FindingSeverity::Critical,
                        FindingStatus::Resolved,
                        &["reviewer-c"],
                    ),
                ],
            ),
        )
        .unwrap();

        assert_eq!(
            state.findings[0].owner_reviewer_node_ids,
            ids(&["reviewer-a", "reviewer-b", "reviewer-c"])
        );
        assert_eq!(state.findings[0].status, FindingStatus::Reopened);
        assert_eq!(state.critical_count, 1);
    }

    #[test]
    fn illegal_severity_mutation_is_rejected() {
        let prior = initial(vec![finding(
            "F-1",
            FindingSeverity::Important,
            FindingStatus::Open,
            &["reviewer-a"],
        )]);
        let error = derive_plan_review_round(
            Some(&prior),
            &ids(&["reviewer-a", "reviewer-b", "reviewer-c"]),
            &submission(
                PlanReviewScope::Scoped,
                PlanRevisionKind::Localized,
                &["reviewer-a"],
                vec![finding(
                    "F-1",
                    FindingSeverity::Critical,
                    FindingStatus::Open,
                    &["reviewer-a"],
                )],
            ),
        )
        .unwrap_err();

        assert_eq!(
            error,
            PlanReviewError::SeverityMutation {
                finding_id: "F-1".to_owned()
            }
        );
    }

    #[test]
    fn unknown_owners_are_rejected() {
        let error = derive_plan_review_round(
            None,
            &ids(&["reviewer-a"]),
            &submission(
                PlanReviewScope::Full,
                PlanRevisionKind::Initial,
                &["reviewer-a"],
                vec![finding(
                    "F-1",
                    FindingSeverity::Important,
                    FindingStatus::New,
                    &["reviewer-missing"],
                )],
            ),
        )
        .unwrap_err();

        assert_eq!(
            error,
            PlanReviewError::UnknownReviewerNodeId("reviewer-missing".to_owned())
        );
    }

    #[test]
    fn carried_finding_owner_absent_from_current_cohort_is_rejected() {
        let prior = initial(vec![finding(
            "F-1",
            FindingSeverity::Important,
            FindingStatus::Open,
            &["reviewer-c"],
        )]);
        let error = derive_plan_review_round(
            Some(&prior),
            &ids(&["reviewer-a", "reviewer-b"]),
            &submission(
                PlanReviewScope::Full,
                PlanRevisionKind::Material,
                &["reviewer-a", "reviewer-b"],
                vec![],
            ),
        )
        .unwrap_err();

        assert_eq!(
            error,
            PlanReviewError::UnknownReviewerNodeId("reviewer-c".to_owned())
        );
    }

    #[test]
    fn owner_subset_is_derived_from_open_blocking_findings() {
        let state = initial(vec![
            finding(
                "F-critical",
                FindingSeverity::Critical,
                FindingStatus::Open,
                &["reviewer-c", "reviewer-a"],
            ),
            finding(
                "F-important",
                FindingSeverity::Important,
                FindingStatus::New,
                &["reviewer-b"],
            ),
            finding(
                "F-minor",
                FindingSeverity::Minor,
                FindingStatus::Open,
                &["reviewer-c"],
            ),
        ]);

        assert_eq!(
            state.next_required_reviewer_node_ids,
            ids(&["reviewer-a", "reviewer-b", "reviewer-c"])
        );
    }

    #[test]
    fn all_owners_must_participate_before_resolution() {
        let prior = initial(vec![finding(
            "F-1",
            FindingSeverity::Important,
            FindingStatus::Open,
            &["reviewer-a", "reviewer-b"],
        )]);
        let incomplete = submission(
            PlanReviewScope::Scoped,
            PlanRevisionKind::Localized,
            &["reviewer-a"],
            vec![finding(
                "F-1",
                FindingSeverity::Important,
                FindingStatus::Resolved,
                &["reviewer-a"],
            )],
        );
        let error = derive_plan_review_round(
            Some(&prior),
            &ids(&["reviewer-a", "reviewer-b", "reviewer-c"]),
            &incomplete,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            PlanReviewError::RequiredReviewerSetMismatch { .. }
        ));

        let state = derive_plan_review_round(
            Some(&prior),
            &ids(&["reviewer-a", "reviewer-b", "reviewer-c"]),
            &submission(
                PlanReviewScope::Scoped,
                PlanRevisionKind::Localized,
                &["reviewer-a", "reviewer-b"],
                vec![finding(
                    "F-1",
                    FindingSeverity::Important,
                    FindingStatus::Resolved,
                    &["reviewer-a", "reviewer-b"],
                )],
            ),
        )
        .unwrap();
        assert_eq!(state.important_count, 0);
        assert_eq!(state.next_action, PlanReviewNextAction::Approved);
    }

    #[test]
    fn scoped_round_accepts_a_new_finding_from_a_required_owner() {
        let prior = initial(vec![finding(
            "F-1",
            FindingSeverity::Important,
            FindingStatus::Open,
            &["reviewer-a"],
        )]);
        let state = next(
            &prior,
            PlanRevisionKind::Localized,
            vec![finding(
                "F-2",
                FindingSeverity::Critical,
                FindingStatus::New,
                &["reviewer-a"],
            )],
        );

        assert_eq!(state.findings.len(), 2);
        assert_eq!(state.critical_count, 1);
        assert_eq!(state.important_count, 1);
    }

    #[test]
    fn minor_only_round_is_approved() {
        let state = initial(vec![finding(
            "F-minor",
            FindingSeverity::Minor,
            FindingStatus::Open,
            &["reviewer-b"],
        )]);

        assert_eq!(state.minor_count, 1);
        assert_eq!(state.next_required_reviewer_node_ids, Vec::<String>::new());
        assert_eq!(state.next_action, PlanReviewNextAction::Approved);
    }

    #[test]
    fn material_and_full_localized_revisions_restore_full_cohort() {
        let prior = initial(vec![finding(
            "F-1",
            FindingSeverity::Important,
            FindingStatus::Open,
            &["reviewer-a"],
        )]);
        let cohort = ids(&["reviewer-a", "reviewer-b", "reviewer-c"]);
        for revision_kind in [PlanRevisionKind::Material, PlanRevisionKind::Localized] {
            let state = derive_plan_review_round(
                Some(&prior),
                &cohort,
                &submission(
                    PlanReviewScope::Full,
                    revision_kind,
                    &["reviewer-c", "reviewer-a", "reviewer-b"],
                    vec![],
                ),
            )
            .unwrap();
            assert_eq!(state.reviewed_reviewer_node_ids, cohort);
        }
    }

    #[test]
    fn first_full_round_establishes_baseline_without_stagnation() {
        let state = initial(vec![finding(
            "F-1",
            FindingSeverity::Critical,
            FindingStatus::Open,
            &["reviewer-a"],
        )]);

        assert!(!state.net_improvement);
        assert_eq!(state.stagnation_count, 0);
        assert!(!state.rewrite_used);
        assert_eq!(state.next_action, PlanReviewNextAction::ContinueReview);
    }

    #[test]
    fn lower_blocking_total_without_critical_increase_is_improvement() {
        let prior = initial(vec![
            finding(
                "F-critical",
                FindingSeverity::Critical,
                FindingStatus::Open,
                &["reviewer-a"],
            ),
            finding(
                "F-important",
                FindingSeverity::Important,
                FindingStatus::Open,
                &["reviewer-a"],
            ),
        ]);
        let state = next(
            &prior,
            PlanRevisionKind::Localized,
            vec![finding(
                "F-important",
                FindingSeverity::Important,
                FindingStatus::Resolved,
                &["reviewer-a"],
            )],
        );

        assert!(state.net_improvement);
        assert_eq!(state.critical_count, 1);
        assert_eq!(state.important_count, 0);
        assert_eq!(state.stagnation_count, 0);
    }

    #[test]
    fn new_critical_is_not_improvement_even_when_total_falls() {
        let prior = initial(vec![
            finding(
                "F-important-1",
                FindingSeverity::Important,
                FindingStatus::Open,
                &["reviewer-a"],
            ),
            finding(
                "F-important-2",
                FindingSeverity::Important,
                FindingStatus::Open,
                &["reviewer-a"],
            ),
        ]);
        let state = next(
            &prior,
            PlanRevisionKind::Localized,
            vec![
                finding(
                    "F-important-1",
                    FindingSeverity::Important,
                    FindingStatus::Resolved,
                    &["reviewer-a"],
                ),
                finding(
                    "F-important-2",
                    FindingSeverity::Important,
                    FindingStatus::Resolved,
                    &["reviewer-a"],
                ),
                finding(
                    "F-critical",
                    FindingSeverity::Critical,
                    FindingStatus::New,
                    &["reviewer-a"],
                ),
            ],
        );

        assert!(!state.net_improvement);
        assert_eq!((state.critical_count, state.important_count), (1, 0));
        assert_eq!(state.stagnation_count, 1);
    }

    #[test]
    fn two_non_improving_rounds_require_one_holistic_rewrite() {
        let baseline = initial(vec![finding(
            "F-1",
            FindingSeverity::Important,
            FindingStatus::Open,
            &["reviewer-a"],
        )]);
        let stagnant_one = next(&baseline, PlanRevisionKind::Localized, vec![]);
        let stagnant_two = next(&stagnant_one, PlanRevisionKind::Localized, vec![]);

        assert_eq!(stagnant_two.stagnation_count, 2);
        assert!(!stagnant_two.rewrite_used);
        assert_eq!(
            stagnant_two.next_action,
            PlanReviewNextAction::HolisticRewriteRequired
        );
    }

    #[test]
    fn post_rewrite_round_compares_with_pre_rewrite_counts() {
        let baseline = initial(vec![finding(
            "F-1",
            FindingSeverity::Important,
            FindingStatus::Open,
            &["reviewer-a"],
        )]);
        let stagnant_one = next(&baseline, PlanRevisionKind::Localized, vec![]);
        let stagnant_two = next(&stagnant_one, PlanRevisionKind::Localized, vec![]);
        let post_rewrite = derive_plan_review_round(
            Some(&stagnant_two),
            &ids(&["reviewer-a", "reviewer-b", "reviewer-c"]),
            &submission(
                PlanReviewScope::Full,
                PlanRevisionKind::HolisticRewrite,
                &["reviewer-a", "reviewer-b", "reviewer-c"],
                vec![],
            ),
        )
        .unwrap();

        assert!(!post_rewrite.net_improvement);
        assert_eq!(post_rewrite.stagnation_count, 1);
        assert!(post_rewrite.rewrite_used);
        assert_eq!(
            post_rewrite.next_action,
            PlanReviewNextAction::ContinueReview
        );
    }

    #[test]
    fn second_stagnation_pair_requires_user_decision() {
        let baseline = initial(vec![finding(
            "F-1",
            FindingSeverity::Important,
            FindingStatus::Open,
            &["reviewer-a"],
        )]);
        let stagnant_one = next(&baseline, PlanRevisionKind::Localized, vec![]);
        let stagnant_two = next(&stagnant_one, PlanRevisionKind::Localized, vec![]);
        let post_rewrite = derive_plan_review_round(
            Some(&stagnant_two),
            &ids(&["reviewer-a", "reviewer-b", "reviewer-c"]),
            &submission(
                PlanReviewScope::Full,
                PlanRevisionKind::HolisticRewrite,
                &["reviewer-a", "reviewer-b", "reviewer-c"],
                vec![],
            ),
        )
        .unwrap();
        let blocked = next(&post_rewrite, PlanRevisionKind::Localized, vec![]);

        assert_eq!(blocked.stagnation_count, 2);
        assert!(blocked.rewrite_used);
        assert_eq!(
            blocked.next_action,
            PlanReviewNextAction::UserDecisionRequired
        );
    }

    #[test]
    fn requirements_lineage_reset_requires_reason_and_resets_baseline() {
        let baseline = initial(vec![finding(
            "F-1",
            FindingSeverity::Important,
            FindingStatus::Open,
            &["reviewer-a"],
        )]);
        let stagnant_one = next(&baseline, PlanRevisionKind::Localized, vec![]);
        let stagnant_two = next(&stagnant_one, PlanRevisionKind::Localized, vec![]);

        let mut missing_reason = submission(
            PlanReviewScope::Full,
            PlanRevisionKind::Initial,
            &["reviewer-a", "reviewer-b", "reviewer-c"],
            vec![],
        );
        missing_reason.lineage_reset_reason = Some("   ".to_owned());
        assert_eq!(
            derive_plan_review_round(
                Some(&stagnant_two),
                &ids(&["reviewer-a", "reviewer-b", "reviewer-c"]),
                &missing_reason,
            )
            .unwrap_err(),
            PlanReviewError::InvalidLineageResetReason
        );

        let mut reset = missing_reason;
        reset.lineage_reset_reason = Some("user approved changed requirements".to_owned());
        reset.finding_updates = vec![finding(
            "F-new",
            FindingSeverity::Important,
            FindingStatus::New,
            &["reviewer-b"],
        )];
        let state = derive_plan_review_round(
            Some(&stagnant_two),
            &ids(&["reviewer-a", "reviewer-b", "reviewer-c"]),
            &reset,
        )
        .unwrap();

        assert!(!state.net_improvement);
        assert_eq!(state.stagnation_count, 0);
        assert!(!state.rewrite_used);
        assert_eq!(state.findings.len(), 1);
        assert_eq!(state.findings[0].finding_id, "F-new");
    }
}
