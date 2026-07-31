use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::db::entities::delegation_task_run::DelegationRunStatus;
use crate::db::entities::delegation_workflow_gate_settlement::GateSettlementOutcome;

use super::plan_review::PlanReviewNextAction;
use super::types::{ManifestRevisionKind, ManifestWorkflowState, WorkflowBlockCause};

const FINGERPRINT_VERSION: &str = "workflow_recovery_v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkflowRecoverySnapshot {
    pub workflow_id: String,
    pub parent_conversation_id: i32,
    pub workflow_kind: String,
    pub schema_version: u64,
    pub capability_version: String,
    pub header_state: ManifestWorkflowState,
    pub active_manifest_revision: u64,
    pub structural_revision: u64,
    pub active_manifest_revision_kind: ManifestRevisionKind,
    pub active_manifest_source_revision: Option<u64>,
    pub supersedes_approved_revision: Option<u64>,
    pub active_manifest_digest: Option<String>,
    pub manifest_state: Option<ManifestWorkflowState>,
    pub normalized_manifest_state: Option<ManifestWorkflowState>,
    pub header_manifest_state_match: bool,
    pub active_manifest_valid: bool,
    pub fingerprints_valid: bool,
    pub design_fingerprint: String,
    pub plan_fingerprint: String,
    pub plan_target_rel_path: String,
    pub design: Option<WorkflowRecoveryDocumentIdentity>,
    pub plan: Option<WorkflowRecoveryDocumentIdentity>,
    pub current_plan_gate_id: Option<String>,
    pub active_plan_author: Option<WorkflowRecoveryPlanIdentity>,
    pub required_plan_reviewers: Vec<WorkflowRecoveryPlanIdentity>,
    pub latest_plan_gate: Option<WorkflowRecoveryPlanGateEvidence>,
    pub current_plan_gate: Option<WorkflowRecoveryPlanGateEvidence>,
    pub binding_lifecycle: Vec<WorkflowRecoveryBindingLifecycle>,
    pub active_runs: Vec<WorkflowRecoveryActiveRun>,
    pub frozen_task_cohorts: Vec<WorkflowRecoveryFrozenTaskCohort>,
    pub binding_evidence_consistent: bool,
    pub latest_run_supersession_valid: bool,
    pub contradictory_durable_state: bool,
    pub block_cause: WorkflowBlockCause,
    pub block_source_manifest_revision: Option<u64>,
    pub plan_lineage_reset_pending: bool,
    pub displayed_reset_reason_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkflowRecoveryDocumentIdentity {
    pub rel_path: String,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkflowRecoveryPlanIdentity {
    pub node_id: String,
    pub work_unit_key: String,
    pub agent_type: String,
    pub profile_id: Option<String>,
    pub active: bool,
    pub observed: bool,
    pub latest_task_id: Option<String>,
    pub latest_status: Option<DelegationRunStatus>,
    pub summary_validated: bool,
    pub artifact_digest: Option<String>,
    pub gate_id: Option<String>,
    pub gate_cycle: Option<i64>,
    pub reviewed_task_id: Option<String>,
    pub evidence_consistent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkflowRecoveryPlanGateEvidence {
    pub gate_id: String,
    pub gate_cycle: i64,
    pub outcome: GateSettlementOutcome,
    pub content_fingerprint: String,
    pub critical_count: i64,
    pub important_count: i64,
    pub minor_count: i64,
    pub next_action: Option<PlanReviewNextAction>,
    pub covered_author_task_id: Option<String>,
    pub covered_plan_digest: Option<String>,
    pub required_reviewer_node_ids: Vec<String>,
    pub reviewer_evidence_count: usize,
    pub evidence_consistent: bool,
    pub lineage_reset_consumed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkflowRecoveryBindingLifecycle {
    pub node_id: String,
    pub work_unit_key: String,
    pub role: String,
    pub agent_type: String,
    pub profile_id: Option<String>,
    pub phase_id: String,
    pub task_index: Option<u32>,
    pub introduced_revision: u64,
    pub retired_revision: Option<u64>,
    pub observed: bool,
    pub retained_observed: bool,
    pub frozen: bool,
    pub node_outcome: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkflowRecoveryActiveRun {
    pub task_id: String,
    pub node_id: String,
    pub status: DelegationRunStatus,
    pub generation: i64,
    pub lineage_ordinal: i64,
    pub replaced_task_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkflowRecoveryFrozenTaskCohort {
    pub task_index: u32,
    pub implementer_node_id: String,
    pub reviewer_node_ids: Vec<String>,
    pub route_complete: bool,
    pub unresolved: bool,
    pub evidence_consistent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowRecoveryDecision {
    pub workflow_id: String,
    pub source_state_fingerprint: String,
    pub disposition: WorkflowRecoveryDisposition,
    pub confirmation: WorkflowRecoveryConfirmation,
    pub cause_code: WorkflowRecoveryCauseCode,
    pub risk_class: WorkflowRecoveryRiskClass,
    reset_reason_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowRecoveryDisposition {
    Recover {
        target_state: ManifestWorkflowState,
    },
    ResetPlanLineage,
    Stop {
        code: WorkflowRecoveryStopCode,
        blockers: Vec<WorkflowRecoveryBlocker>,
    },
    InconsistentDurableState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRecoveryBlocker {
    ActiveRun,
    ReservingRun,
    UnresolvedFrozenTaskCohort,
    HeaderManifestStateMismatch,
    InvalidActiveManifest,
    StalePlanGateEvidence,
    AuthorEvidenceMismatch,
    ReviewerEvidenceMismatch,
    BindingEvidenceMismatch,
    LatestRunSupersessionInvalid,
}

impl WorkflowRecoveryBlocker {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::ActiveRun => "active_run",
            Self::ReservingRun => "reserving_run",
            Self::UnresolvedFrozenTaskCohort => "unresolved_frozen_task_cohort",
            Self::HeaderManifestStateMismatch => "header_manifest_state_mismatch",
            Self::InvalidActiveManifest => "invalid_active_manifest",
            Self::StalePlanGateEvidence => "stale_plan_gate_evidence",
            Self::AuthorEvidenceMismatch => "author_evidence_mismatch",
            Self::ReviewerEvidenceMismatch => "reviewer_evidence_mismatch",
            Self::BindingEvidenceMismatch => "binding_evidence_mismatch",
            Self::LatestRunSupersessionInvalid => "latest_run_supersession_invalid",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRecoveryStopCode {
    RecoveryNotAvailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRecoveryConfirmation {
    NotRequired,
    Required,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRecoveryCauseCode {
    LegacyBlockWithCurrentPlanApproval,
    LegacyBlockWithCurrentPlan,
    LegacyBlockWithoutPlan,
    PlanUserDecisionRequired,
    PlanGateBlocked,
    ExplicitManifestBlock,
    UnresolvedTaskCohort,
    DurableStateInconsistent,
}

impl WorkflowRecoveryCauseCode {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::LegacyBlockWithCurrentPlanApproval => "legacy_block_with_current_plan_approval",
            Self::LegacyBlockWithCurrentPlan => "legacy_block_with_current_plan",
            Self::LegacyBlockWithoutPlan => "legacy_block_without_plan",
            Self::PlanUserDecisionRequired => "plan_user_decision_required",
            Self::PlanGateBlocked => "plan_gate_blocked",
            Self::ExplicitManifestBlock => "explicit_manifest_block",
            Self::UnresolvedTaskCohort => "unresolved_task_cohort",
            Self::DurableStateInconsistent => "durable_state_inconsistent",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowRecoveryRiskClass {
    Normal,
    PlanLineageReset,
    LegacyUnknownOrigin,
    DurableStateRisk,
}

impl WorkflowRecoveryRiskClass {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::PlanLineageReset => "plan_lineage_reset",
            Self::LegacyUnknownOrigin => "legacy_unknown_origin",
            Self::DurableStateRisk => "durable_state_risk",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowRecoveryProjection {
    pub disposition: String,
    pub proposed_action: Option<String>,
    pub target_state: Option<ManifestWorkflowState>,
    pub cause_code: String,
    pub risk_class: String,
    pub authorization_required: bool,
    pub blockers: Vec<String>,
}

impl WorkflowRecoveryDecision {
    pub fn requires_authorization(&self) -> bool {
        self.confirmation == WorkflowRecoveryConfirmation::Required
            && matches!(
                self.disposition,
                WorkflowRecoveryDisposition::Recover { .. }
                    | WorkflowRecoveryDisposition::ResetPlanLineage
            )
    }

    pub fn proposed_action(&self) -> Option<&'static str> {
        match self.disposition {
            WorkflowRecoveryDisposition::Recover { .. } => Some("recover_workflow"),
            WorkflowRecoveryDisposition::ResetPlanLineage => Some("reset_plan_lineage"),
            WorkflowRecoveryDisposition::Stop { .. }
            | WorkflowRecoveryDisposition::InconsistentDurableState => None,
        }
    }

    pub fn target_state(&self) -> Option<ManifestWorkflowState> {
        match self.disposition {
            WorkflowRecoveryDisposition::Recover { target_state } => Some(target_state),
            WorkflowRecoveryDisposition::ResetPlanLineage
            | WorkflowRecoveryDisposition::Stop { .. }
            | WorkflowRecoveryDisposition::InconsistentDurableState => None,
        }
    }

    pub fn action_payload(&self) -> Option<serde_json::Value> {
        match self.disposition {
            WorkflowRecoveryDisposition::Recover { target_state } => {
                Some(serde_json::json!({ "target_state": target_state }))
            }
            WorkflowRecoveryDisposition::ResetPlanLineage => {
                let hash = self
                    .reset_reason_hash
                    .as_deref()
                    .expect("reset decision always fingerprints a displayed reason");
                Some(serde_json::json!({ "displayed_reason_sha256": hash }))
            }
            WorkflowRecoveryDisposition::Stop { .. }
            | WorkflowRecoveryDisposition::InconsistentDurableState => None,
        }
    }

    pub fn projection(&self) -> WorkflowRecoveryProjection {
        let blockers = match &self.disposition {
            WorkflowRecoveryDisposition::Stop { blockers, .. } => blockers
                .iter()
                .map(|blocker| blocker.as_str().to_string())
                .collect(),
            _ => Vec::new(),
        };
        let disposition = match self.disposition {
            WorkflowRecoveryDisposition::Recover { .. }
            | WorkflowRecoveryDisposition::ResetPlanLineage => "confirmation_required",
            WorkflowRecoveryDisposition::Stop { .. } => "blocked",
            WorkflowRecoveryDisposition::InconsistentDurableState => "inconsistent_durable_state",
        };
        WorkflowRecoveryProjection {
            disposition: disposition.into(),
            proposed_action: self.proposed_action().map(str::to_string),
            target_state: self.target_state(),
            cause_code: self.cause_code.as_str().into(),
            risk_class: self.risk_class.as_str().into(),
            authorization_required: self.requires_authorization(),
            blockers,
        }
    }
}

pub fn hash_displayed_reset_reason(reason: &str) -> String {
    hex_lower(&Sha256::digest(reason.as_bytes()))
}

pub fn decide_workflow_recovery(source: &WorkflowRecoverySnapshot) -> WorkflowRecoveryDecision {
    let canonical_fingerprint = source_state_fingerprint(source);
    let risk_class = risk_class(source);
    let mut blockers = Vec::new();

    if source
        .active_runs
        .iter()
        .any(|run| run.status == DelegationRunStatus::Running)
    {
        blockers.push(WorkflowRecoveryBlocker::ActiveRun);
    }
    if source
        .active_runs
        .iter()
        .any(|run| run.status == DelegationRunStatus::Reserving)
    {
        blockers.push(WorkflowRecoveryBlocker::ReservingRun);
    }
    if source
        .frozen_task_cohorts
        .iter()
        .any(|cohort| cohort.unresolved || !cohort.route_complete)
    {
        blockers.push(WorkflowRecoveryBlocker::UnresolvedFrozenTaskCohort);
    }
    if !source.header_manifest_state_match {
        blockers.push(WorkflowRecoveryBlocker::HeaderManifestStateMismatch);
    }
    if !source.active_manifest_valid {
        blockers.push(WorkflowRecoveryBlocker::InvalidActiveManifest);
    }
    if !source.fingerprints_valid
        || source
            .latest_plan_gate
            .as_ref()
            .is_some_and(|gate| !gate.evidence_consistent)
        || source
            .current_plan_gate
            .as_ref()
            .is_some_and(|gate| !gate.evidence_consistent)
    {
        blockers.push(WorkflowRecoveryBlocker::StalePlanGateEvidence);
    }
    if source
        .active_plan_author
        .as_ref()
        .is_some_and(|author| !author.evidence_consistent)
    {
        blockers.push(WorkflowRecoveryBlocker::AuthorEvidenceMismatch);
    }
    if source
        .required_plan_reviewers
        .iter()
        .any(|reviewer| !reviewer.evidence_consistent)
    {
        blockers.push(WorkflowRecoveryBlocker::ReviewerEvidenceMismatch);
    }
    if !source.binding_evidence_consistent
        || source
            .frozen_task_cohorts
            .iter()
            .any(|cohort| !cohort.evidence_consistent)
    {
        blockers.push(WorkflowRecoveryBlocker::BindingEvidenceMismatch);
    }
    if !source.latest_run_supersession_valid {
        blockers.push(WorkflowRecoveryBlocker::LatestRunSupersessionInvalid);
    }

    if !blockers.is_empty() || source.header_state != ManifestWorkflowState::Blocked {
        return decision(
            source,
            canonical_fingerprint,
            WorkflowRecoveryDisposition::Stop {
                code: WorkflowRecoveryStopCode::RecoveryNotAvailable,
                blockers,
            },
            WorkflowRecoveryConfirmation::NotRequired,
            cause_code(source, false),
            risk_class,
        );
    }
    if source.contradictory_durable_state {
        return decision(
            source,
            canonical_fingerprint,
            WorkflowRecoveryDisposition::InconsistentDurableState,
            WorkflowRecoveryConfirmation::NotRequired,
            WorkflowRecoveryCauseCode::DurableStateInconsistent,
            WorkflowRecoveryRiskClass::DurableStateRisk,
        );
    }

    let latest_gate_requests_lineage_reset = source
        .latest_plan_gate
        .as_ref()
        .is_some_and(|gate| gate.next_action == Some(PlanReviewNextAction::UserDecisionRequired));
    if source.plan_lineage_reset_pending
        || latest_gate_requests_lineage_reset
        || source.block_cause == WorkflowBlockCause::PlanUserDecisionRequired
    {
        if source.plan_lineage_reset_pending
            && latest_gate_requests_lineage_reset
            && source.displayed_reset_reason_hash.is_some()
        {
            return decision(
                source,
                canonical_fingerprint,
                WorkflowRecoveryDisposition::ResetPlanLineage,
                WorkflowRecoveryConfirmation::Required,
                WorkflowRecoveryCauseCode::PlanUserDecisionRequired,
                WorkflowRecoveryRiskClass::PlanLineageReset,
            );
        }
        return decision(
            source,
            canonical_fingerprint,
            WorkflowRecoveryDisposition::Stop {
                code: WorkflowRecoveryStopCode::RecoveryNotAvailable,
                blockers: vec![WorkflowRecoveryBlocker::StalePlanGateEvidence],
            },
            WorkflowRecoveryConfirmation::NotRequired,
            WorkflowRecoveryCauseCode::PlanUserDecisionRequired,
            WorkflowRecoveryRiskClass::PlanLineageReset,
        );
    }

    let target_state = if source.plan.is_none() {
        ManifestWorkflowState::Skeleton
    } else if exact_current_plan_approval(source) {
        ManifestWorkflowState::Approved
    } else {
        ManifestWorkflowState::Estimated
    };
    decision(
        source,
        canonical_fingerprint,
        WorkflowRecoveryDisposition::Recover { target_state },
        WorkflowRecoveryConfirmation::Required,
        cause_code(source, target_state == ManifestWorkflowState::Approved),
        risk_class,
    )
}

fn decision(
    source: &WorkflowRecoverySnapshot,
    source_state_fingerprint: String,
    disposition: WorkflowRecoveryDisposition,
    confirmation: WorkflowRecoveryConfirmation,
    cause_code: WorkflowRecoveryCauseCode,
    risk_class: WorkflowRecoveryRiskClass,
) -> WorkflowRecoveryDecision {
    WorkflowRecoveryDecision {
        workflow_id: source.workflow_id.clone(),
        source_state_fingerprint,
        disposition,
        confirmation,
        cause_code,
        risk_class,
        reset_reason_hash: source.displayed_reset_reason_hash.clone(),
    }
}

fn risk_class(source: &WorkflowRecoverySnapshot) -> WorkflowRecoveryRiskClass {
    if source.plan_lineage_reset_pending
        || source.latest_plan_gate.as_ref().is_some_and(|gate| {
            gate.next_action == Some(PlanReviewNextAction::UserDecisionRequired)
        })
    {
        return WorkflowRecoveryRiskClass::PlanLineageReset;
    }
    match source.block_cause {
        WorkflowBlockCause::LegacyUnknown => WorkflowRecoveryRiskClass::LegacyUnknownOrigin,
        WorkflowBlockCause::PlanUserDecisionRequired => WorkflowRecoveryRiskClass::PlanLineageReset,
        WorkflowBlockCause::DurableStateInconsistent => WorkflowRecoveryRiskClass::DurableStateRisk,
        WorkflowBlockCause::PlanGateBlocked
        | WorkflowBlockCause::ExplicitManifestBlock
        | WorkflowBlockCause::UnresolvedTaskCohort => WorkflowRecoveryRiskClass::Normal,
    }
}

fn cause_code(
    source: &WorkflowRecoverySnapshot,
    exact_plan_approval: bool,
) -> WorkflowRecoveryCauseCode {
    if source.plan_lineage_reset_pending
        || source.latest_plan_gate.as_ref().is_some_and(|gate| {
            gate.next_action == Some(PlanReviewNextAction::UserDecisionRequired)
        })
    {
        return WorkflowRecoveryCauseCode::PlanUserDecisionRequired;
    }
    match source.block_cause {
        WorkflowBlockCause::LegacyUnknown if exact_plan_approval => {
            WorkflowRecoveryCauseCode::LegacyBlockWithCurrentPlanApproval
        }
        WorkflowBlockCause::LegacyUnknown if source.plan.is_some() => {
            WorkflowRecoveryCauseCode::LegacyBlockWithCurrentPlan
        }
        WorkflowBlockCause::LegacyUnknown => WorkflowRecoveryCauseCode::LegacyBlockWithoutPlan,
        WorkflowBlockCause::PlanUserDecisionRequired => {
            WorkflowRecoveryCauseCode::PlanUserDecisionRequired
        }
        WorkflowBlockCause::PlanGateBlocked => WorkflowRecoveryCauseCode::PlanGateBlocked,
        WorkflowBlockCause::ExplicitManifestBlock => {
            WorkflowRecoveryCauseCode::ExplicitManifestBlock
        }
        WorkflowBlockCause::UnresolvedTaskCohort => WorkflowRecoveryCauseCode::UnresolvedTaskCohort,
        WorkflowBlockCause::DurableStateInconsistent => {
            WorkflowRecoveryCauseCode::DurableStateInconsistent
        }
    }
}

fn exact_current_plan_approval(source: &WorkflowRecoverySnapshot) -> bool {
    let (Some(plan), Some(author), Some(gate)) = (
        source.plan.as_ref(),
        source.active_plan_author.as_ref(),
        source.current_plan_gate.as_ref(),
    ) else {
        return false;
    };
    let Some(author_task_id) = author.latest_task_id.as_deref() else {
        return false;
    };
    if gate.outcome != GateSettlementOutcome::Approved
        || gate.next_action != Some(PlanReviewNextAction::Approved)
        || gate.content_fingerprint.is_empty()
        || gate.content_fingerprint != source.plan_fingerprint
        || gate.critical_count != 0
        || gate.important_count != 0
        || gate.covered_plan_digest.as_deref() != Some(plan.digest.as_str())
        || gate.covered_author_task_id.as_deref() != Some(author_task_id)
        || gate.reviewer_evidence_count == 0
        || gate.reviewer_evidence_count != source.required_plan_reviewers.len()
        || source.current_plan_gate_id.as_deref() != Some(gate.gate_id.as_str())
        || !current_plan_identity(author, plan)
    {
        return false;
    }

    let mut expected = source
        .required_plan_reviewers
        .iter()
        .map(|reviewer| reviewer.node_id.as_str())
        .collect::<Vec<_>>();
    let mut covered = gate
        .required_reviewer_node_ids
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    expected.sort_unstable();
    covered.sort_unstable();
    if expected != covered {
        return false;
    }
    source.required_plan_reviewers.iter().all(|reviewer| {
        current_plan_identity(reviewer, plan)
            && reviewer.gate_id.as_deref() == Some(gate.gate_id.as_str())
            && reviewer.gate_cycle == Some(gate.gate_cycle)
            && reviewer.reviewed_task_id.as_deref() == Some(author_task_id)
    })
}

fn current_plan_identity(
    identity: &WorkflowRecoveryPlanIdentity,
    plan: &WorkflowRecoveryDocumentIdentity,
) -> bool {
    identity.active
        && identity.observed
        && identity.latest_status == Some(DelegationRunStatus::Completed)
        && identity.summary_validated
        && identity.artifact_digest.as_deref() == Some(plan.digest.as_str())
}

#[derive(Serialize)]
struct CanonicalWorkflowRecoverySource<'a> {
    version: &'static str,
    source: &'a WorkflowRecoverySnapshot,
}

fn source_state_fingerprint(source: &WorkflowRecoverySnapshot) -> String {
    let mut canonical = source.clone();
    canonical
        .required_plan_reviewers
        .sort_by(|a, b| a.node_id.cmp(&b.node_id));
    canonical
        .binding_lifecycle
        .sort_by(|a, b| a.node_id.cmp(&b.node_id));
    canonical
        .active_runs
        .sort_by(|a, b| a.task_id.cmp(&b.task_id));
    canonical
        .frozen_task_cohorts
        .sort_by_key(|cohort| cohort.task_index);
    for cohort in &mut canonical.frozen_task_cohorts {
        cohort.reviewer_node_ids.sort();
    }
    if let Some(gate) = canonical.latest_plan_gate.as_mut() {
        gate.required_reviewer_node_ids.sort();
    }
    if let Some(gate) = canonical.current_plan_gate.as_mut() {
        gate.required_reviewer_node_ids.sort();
    }
    let bytes = serde_json::to_vec(&CanonicalWorkflowRecoverySource {
        version: FINGERPRINT_VERSION,
        source: &canonical,
    })
    .expect("workflow recovery fingerprint input serializes");
    format!(
        "{FINGERPRINT_VERSION}:{}",
        hex_lower(&Sha256::digest(bytes))
    )
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod workflow_recovery_policy {
    use super::*;
    use crate::acp::delegation::workflow::plan_review::PlanReviewNextAction;
    use crate::acp::delegation::workflow::types::{
        ManifestRevisionKind, ManifestWorkflowState, WorkflowBlockCause,
    };
    use crate::db::entities::delegation_task_run::DelegationRunStatus;
    use crate::db::entities::delegation_workflow_gate_settlement::GateSettlementOutcome;

    fn document(path: &str, digest: &str) -> WorkflowRecoveryDocumentIdentity {
        WorkflowRecoveryDocumentIdentity {
            rel_path: path.into(),
            digest: digest.into(),
        }
    }

    fn author() -> WorkflowRecoveryPlanIdentity {
        WorkflowRecoveryPlanIdentity {
            node_id: "plan-author".into(),
            work_unit_key: "plan|author".into(),
            agent_type: "codex".into(),
            profile_id: None,
            active: true,
            observed: true,
            latest_task_id: Some("author-task-1".into()),
            latest_status: Some(DelegationRunStatus::Completed),
            summary_validated: true,
            artifact_digest: Some("sha256:plan".into()),
            gate_id: None,
            gate_cycle: None,
            reviewed_task_id: None,
            evidence_consistent: true,
        }
    }

    fn reviewer(node_id: &str) -> WorkflowRecoveryPlanIdentity {
        WorkflowRecoveryPlanIdentity {
            node_id: node_id.into(),
            work_unit_key: format!("plan|reviewer|{node_id}"),
            agent_type: "codex".into(),
            profile_id: None,
            active: true,
            observed: true,
            latest_task_id: Some(format!("{node_id}-task-1")),
            latest_status: Some(DelegationRunStatus::Completed),
            summary_validated: true,
            artifact_digest: Some("sha256:plan".into()),
            gate_id: Some("plan-gate".into()),
            gate_cycle: Some(4),
            reviewed_task_id: Some("author-task-1".into()),
            evidence_consistent: true,
        }
    }

    fn approved_snapshot() -> WorkflowRecoverySnapshot {
        let mut snapshot = WorkflowRecoverySnapshot {
            workflow_id: "workflow-1".into(),
            parent_conversation_id: 42,
            workflow_kind: "brainstorm_to_delivery".into(),
            schema_version: 2,
            capability_version: "workflow_manifest_v2".into(),
            header_state: ManifestWorkflowState::Blocked,
            active_manifest_revision: 8,
            structural_revision: 7,
            active_manifest_revision_kind: ManifestRevisionKind::StateOnly,
            active_manifest_source_revision: Some(7),
            supersedes_approved_revision: Some(6),
            active_manifest_digest: Some("manifest-digest".into()),
            manifest_state: Some(ManifestWorkflowState::Blocked),
            normalized_manifest_state: Some(ManifestWorkflowState::Blocked),
            header_manifest_state_match: true,
            active_manifest_valid: true,
            fingerprints_valid: true,
            design_fingerprint: "design-fingerprint".into(),
            plan_fingerprint: "plan-fingerprint".into(),
            plan_target_rel_path: "docs/plan.md".into(),
            design: Some(document("docs/design.md", "sha256:design")),
            plan: Some(document("docs/plan.md", "sha256:plan")),
            current_plan_gate_id: Some("plan-gate".into()),
            active_plan_author: Some(author()),
            required_plan_reviewers: vec![reviewer("plan-reviewer-a")],
            latest_plan_gate: Some(WorkflowRecoveryPlanGateEvidence {
                gate_id: "plan-gate".into(),
                gate_cycle: 4,
                outcome: GateSettlementOutcome::Approved,
                content_fingerprint: "plan-fingerprint".into(),
                critical_count: 0,
                important_count: 0,
                minor_count: 0,
                next_action: Some(PlanReviewNextAction::Approved),
                covered_author_task_id: Some("author-task-1".into()),
                covered_plan_digest: Some("sha256:plan".into()),
                required_reviewer_node_ids: vec!["plan-reviewer-a".into()],
                reviewer_evidence_count: 1,
                evidence_consistent: true,
                lineage_reset_consumed: false,
            }),
            current_plan_gate: None,
            binding_lifecycle: vec![WorkflowRecoveryBindingLifecycle {
                node_id: "task-1-impl".into(),
                work_unit_key: "task|1|implementer|grok|none".into(),
                role: "implementer".into(),
                agent_type: "grok".into(),
                profile_id: None,
                phase_id: "tasks".into(),
                task_index: Some(1),
                introduced_revision: 1,
                retired_revision: None,
                observed: false,
                retained_observed: false,
                frozen: false,
                node_outcome: None,
            }],
            active_runs: Vec::new(),
            frozen_task_cohorts: Vec::new(),
            binding_evidence_consistent: true,
            latest_run_supersession_valid: true,
            contradictory_durable_state: false,
            block_cause: WorkflowBlockCause::LegacyUnknown,
            block_source_manifest_revision: Some(7),
            plan_lineage_reset_pending: false,
            displayed_reset_reason_hash: None,
        };
        snapshot.current_plan_gate = snapshot.latest_plan_gate.clone();
        snapshot
    }

    fn assert_recover(
        decision: &WorkflowRecoveryDecision,
        target: ManifestWorkflowState,
        cause: WorkflowRecoveryCauseCode,
        risk: WorkflowRecoveryRiskClass,
    ) {
        assert_eq!(
            decision.disposition,
            WorkflowRecoveryDisposition::Recover {
                target_state: target
            }
        );
        assert_eq!(
            decision.confirmation,
            WorkflowRecoveryConfirmation::Required
        );
        assert_eq!(decision.cause_code, cause);
        assert_eq!(decision.risk_class, risk);
        assert_eq!(
            decision.action_payload(),
            Some(serde_json::json!({ "target_state": target }))
        );
        assert!(decision.requires_authorization());
    }

    #[test]
    fn workflow_recovery_target_matrix() {
        let approved = decide_workflow_recovery(&approved_snapshot());
        assert_recover(
            &approved,
            ManifestWorkflowState::Approved,
            WorkflowRecoveryCauseCode::LegacyBlockWithCurrentPlanApproval,
            WorkflowRecoveryRiskClass::LegacyUnknownOrigin,
        );
        assert_eq!(
            approved.projection(),
            WorkflowRecoveryProjection {
                disposition: "confirmation_required".into(),
                proposed_action: Some("recover_workflow".into()),
                target_state: Some(ManifestWorkflowState::Approved),
                cause_code: "legacy_block_with_current_plan_approval".into(),
                risk_class: "legacy_unknown_origin".into(),
                authorization_required: true,
                blockers: Vec::new(),
            }
        );

        let mut historical_current_approval = approved_snapshot();
        let latest = historical_current_approval
            .latest_plan_gate
            .as_mut()
            .unwrap();
        latest.gate_cycle = 5;
        latest.outcome = GateSettlementOutcome::ChangesRequested;
        latest.content_fingerprint = "different-plan-fingerprint".into();
        latest.next_action = Some(PlanReviewNextAction::ContinueReview);
        assert_recover(
            &decide_workflow_recovery(&historical_current_approval),
            ManifestWorkflowState::Approved,
            WorkflowRecoveryCauseCode::LegacyBlockWithCurrentPlanApproval,
            WorkflowRecoveryRiskClass::LegacyUnknownOrigin,
        );

        let mut estimated = approved_snapshot();
        let gate = estimated.current_plan_gate.as_mut().unwrap();
        gate.outcome = GateSettlementOutcome::ChangesRequested;
        gate.next_action = Some(PlanReviewNextAction::ContinueReview);
        assert_recover(
            &decide_workflow_recovery(&estimated),
            ManifestWorkflowState::Estimated,
            WorkflowRecoveryCauseCode::LegacyBlockWithCurrentPlan,
            WorkflowRecoveryRiskClass::LegacyUnknownOrigin,
        );

        let mut skeleton = approved_snapshot();
        skeleton.plan = None;
        skeleton.active_plan_author = None;
        skeleton.required_plan_reviewers.clear();
        skeleton.latest_plan_gate = None;
        skeleton.current_plan_gate = None;
        assert_recover(
            &decide_workflow_recovery(&skeleton),
            ManifestWorkflowState::Skeleton,
            WorkflowRecoveryCauseCode::LegacyBlockWithoutPlan,
            WorkflowRecoveryRiskClass::LegacyUnknownOrigin,
        );
    }

    #[test]
    fn active_runs_unresolved_frozen_cohorts_and_corrupt_evidence_stop_recovery() {
        let base = approved_snapshot();
        let cases = [
            {
                let mut value = base.clone();
                value.active_runs.push(WorkflowRecoveryActiveRun {
                    task_id: "running-task".into(),
                    node_id: "task-1-impl".into(),
                    status: DelegationRunStatus::Running,
                    generation: 1,
                    lineage_ordinal: 1,
                    replaced_task_id: None,
                });
                (value, WorkflowRecoveryBlocker::ActiveRun)
            },
            {
                let mut value = base.clone();
                value.active_runs.push(WorkflowRecoveryActiveRun {
                    task_id: "reserving-task".into(),
                    node_id: "task-1-impl".into(),
                    status: DelegationRunStatus::Reserving,
                    generation: 1,
                    lineage_ordinal: 1,
                    replaced_task_id: None,
                });
                (value, WorkflowRecoveryBlocker::ReservingRun)
            },
            {
                let mut value = base.clone();
                value
                    .frozen_task_cohorts
                    .push(WorkflowRecoveryFrozenTaskCohort {
                        task_index: 1,
                        implementer_node_id: "task-1-impl".into(),
                        reviewer_node_ids: Vec::new(),
                        route_complete: false,
                        unresolved: true,
                        evidence_consistent: true,
                    });
                (value, WorkflowRecoveryBlocker::UnresolvedFrozenTaskCohort)
            },
            {
                let mut value = base.clone();
                value.header_manifest_state_match = false;
                (value, WorkflowRecoveryBlocker::HeaderManifestStateMismatch)
            },
            {
                let mut value = base.clone();
                value.active_manifest_valid = false;
                (value, WorkflowRecoveryBlocker::InvalidActiveManifest)
            },
            {
                let mut value = base.clone();
                value.latest_plan_gate.as_mut().unwrap().evidence_consistent = false;
                (value, WorkflowRecoveryBlocker::StalePlanGateEvidence)
            },
            {
                let mut value = base.clone();
                value
                    .active_plan_author
                    .as_mut()
                    .unwrap()
                    .evidence_consistent = false;
                (value, WorkflowRecoveryBlocker::AuthorEvidenceMismatch)
            },
            {
                let mut value = base.clone();
                value.required_plan_reviewers[0].evidence_consistent = false;
                (value, WorkflowRecoveryBlocker::ReviewerEvidenceMismatch)
            },
            {
                let mut value = base.clone();
                value.binding_evidence_consistent = false;
                (value, WorkflowRecoveryBlocker::BindingEvidenceMismatch)
            },
            {
                let mut value = base.clone();
                value.latest_run_supersession_valid = false;
                (value, WorkflowRecoveryBlocker::LatestRunSupersessionInvalid)
            },
        ];

        for (source, expected) in cases {
            let decision = decide_workflow_recovery(&source);
            assert_eq!(
                decision.disposition,
                WorkflowRecoveryDisposition::Stop {
                    code: WorkflowRecoveryStopCode::RecoveryNotAvailable,
                    blockers: vec![expected.clone()],
                }
            );
            assert_eq!(decision.action_payload(), None);
            assert!(!decision.requires_authorization());
            let projection = decision.projection();
            assert_eq!(projection.target_state, None);
            assert_eq!(projection.proposed_action, None);
            assert!(!projection.authorization_required);
            assert_eq!(projection.blockers, vec![expected.as_str().to_string()]);
        }

        let mut contradictory = base;
        contradictory.contradictory_durable_state = true;
        let decision = decide_workflow_recovery(&contradictory);
        assert_eq!(
            decision.disposition,
            WorkflowRecoveryDisposition::InconsistentDurableState
        );
        assert_eq!(decision.action_payload(), None);
        assert!(!decision.requires_authorization());
    }

    #[test]
    fn user_decision_required_derives_only_reset_plan_lineage() {
        let reason = "Reset the exhausted Plan lineage after the displayed review history.";
        let mut source = approved_snapshot();
        source.block_cause = WorkflowBlockCause::PlanUserDecisionRequired;
        source.plan_lineage_reset_pending = true;
        source.displayed_reset_reason_hash = Some(hash_displayed_reset_reason(reason));
        source.latest_plan_gate.as_mut().unwrap().next_action =
            Some(PlanReviewNextAction::UserDecisionRequired);

        let decision = decide_workflow_recovery(&source);
        assert_eq!(
            decision.disposition,
            WorkflowRecoveryDisposition::ResetPlanLineage
        );
        assert_eq!(
            decision.confirmation,
            WorkflowRecoveryConfirmation::Required
        );
        assert_eq!(
            decision.cause_code,
            WorkflowRecoveryCauseCode::PlanUserDecisionRequired
        );
        assert_eq!(
            decision.risk_class,
            WorkflowRecoveryRiskClass::PlanLineageReset
        );
        assert!(decision.requires_authorization());
        assert_eq!(decision.target_state(), None);
        let payload = decision.action_payload().expect("reset action payload");
        assert_eq!(
            payload,
            serde_json::json!({
                "displayed_reason_sha256": hash_displayed_reset_reason(reason)
            })
        );
        assert!(!payload.to_string().contains(reason));
        assert_eq!(decision.proposed_action(), Some("reset_plan_lineage"));

        let mut legacy_cause = source.clone();
        legacy_cause.block_cause = WorkflowBlockCause::LegacyUnknown;
        let decision = decide_workflow_recovery(&legacy_cause);
        assert_eq!(
            decision.disposition,
            WorkflowRecoveryDisposition::ResetPlanLineage
        );
        assert_eq!(
            decision.cause_code,
            WorkflowRecoveryCauseCode::PlanUserDecisionRequired
        );
        assert_eq!(
            decision.risk_class,
            WorkflowRecoveryRiskClass::PlanLineageReset
        );

        let mut missing_pending_evidence = source;
        missing_pending_evidence.plan_lineage_reset_pending = false;
        let decision = decide_workflow_recovery(&missing_pending_evidence);
        assert!(matches!(
            &decision.disposition,
            WorkflowRecoveryDisposition::Stop {
                blockers,
                ..
            } if blockers == &vec![WorkflowRecoveryBlocker::StalePlanGateEvidence]
        ));
        assert_eq!(decision.proposed_action(), None);
        assert!(!decision.requires_authorization());
    }

    #[test]
    fn stale_gate_author_reviewer_or_digest_evidence_never_derives_approved() {
        let base = approved_snapshot();
        let cases = [
            {
                let mut value = base.clone();
                value
                    .current_plan_gate
                    .as_mut()
                    .unwrap()
                    .content_fingerprint = "stale".into();
                value
            },
            {
                let mut value = base.clone();
                value.plan.as_mut().unwrap().digest = "sha256:new-plan".into();
                value
            },
            {
                let mut value = base.clone();
                value.active_plan_author.as_mut().unwrap().latest_task_id =
                    Some("new-author-task".into());
                value
            },
            {
                let mut value = base.clone();
                value.required_plan_reviewers[0].gate_cycle = Some(3);
                value
            },
            {
                let mut value = base.clone();
                value.required_plan_reviewers[0].reviewed_task_id =
                    Some("stale-author-task".into());
                value
            },
            {
                let mut value = base.clone();
                value
                    .current_plan_gate
                    .as_mut()
                    .unwrap()
                    .reviewer_evidence_count = 0;
                value
            },
        ];
        for source in cases {
            let decision = decide_workflow_recovery(&source);
            assert_eq!(
                decision.disposition,
                WorkflowRecoveryDisposition::Recover {
                    target_state: ManifestWorkflowState::Estimated,
                }
            );
            assert_ne!(
                decision.disposition,
                WorkflowRecoveryDisposition::Recover {
                    target_state: ManifestWorkflowState::Approved,
                }
            );
        }

        let mut contradictory = base;
        contradictory
            .latest_plan_gate
            .as_mut()
            .unwrap()
            .evidence_consistent = false;
        assert!(matches!(
            decide_workflow_recovery(&contradictory).disposition,
            WorkflowRecoveryDisposition::Stop { .. }
        ));
    }

    #[test]
    fn workflow_fingerprint_changes_for_every_policy_relevant_evidence_change() {
        let source = approved_snapshot();
        let baseline = decide_workflow_recovery(&source).source_state_fingerprint;
        assert!(baseline.starts_with("workflow_recovery_v1:"));
        assert_eq!(baseline.len(), 85);
        assert!(baseline[21..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));

        let mut mutations = Vec::new();
        macro_rules! mutated {
            ($field:ident = $value:expr) => {{
                let mut value = source.clone();
                value.$field = $value;
                mutations.push(value);
            }};
        }
        mutated!(workflow_id = "workflow-2".into());
        mutated!(parent_conversation_id = 43);
        mutated!(workflow_kind = "other".into());
        mutated!(schema_version = 3);
        mutated!(capability_version = "workflow_manifest_v3".into());
        mutated!(header_state = ManifestWorkflowState::Approved);
        mutated!(active_manifest_revision = 9);
        mutated!(structural_revision = 8);
        mutated!(active_manifest_revision_kind = ManifestRevisionKind::Publication);
        mutated!(active_manifest_source_revision = None);
        mutated!(supersedes_approved_revision = None);
        mutated!(active_manifest_digest = Some("other-manifest".into()));
        mutated!(manifest_state = Some(ManifestWorkflowState::Approved));
        mutated!(normalized_manifest_state = Some(ManifestWorkflowState::Approved));
        mutated!(header_manifest_state_match = false);
        mutated!(active_manifest_valid = false);
        mutated!(fingerprints_valid = false);
        mutated!(design_fingerprint = "other-design-fingerprint".into());
        mutated!(plan_fingerprint = "other-plan-fingerprint".into());
        mutated!(plan_target_rel_path = "docs/other-plan.md".into());
        mutated!(design = None);
        mutated!(plan = None);
        mutated!(current_plan_gate_id = Some("other-plan-gate".into()));
        mutated!(active_plan_author = None);
        mutated!(required_plan_reviewers = vec![reviewer("plan-reviewer-b")]);
        mutated!(latest_plan_gate = None);
        mutated!(current_plan_gate = None);
        mutated!(binding_lifecycle = Vec::new());
        mutated!(
            active_runs = vec![WorkflowRecoveryActiveRun {
                task_id: "run-2".into(),
                node_id: "task-1-impl".into(),
                status: DelegationRunStatus::Running,
                generation: 2,
                lineage_ordinal: 2,
                replaced_task_id: Some("run-1".into()),
            }]
        );
        mutated!(
            frozen_task_cohorts = vec![WorkflowRecoveryFrozenTaskCohort {
                task_index: 1,
                implementer_node_id: "task-1-impl".into(),
                reviewer_node_ids: vec!["task-1-reviewer".into()],
                route_complete: true,
                unresolved: false,
                evidence_consistent: true,
            }]
        );
        mutated!(binding_evidence_consistent = false);
        mutated!(latest_run_supersession_valid = false);
        mutated!(contradictory_durable_state = true);
        mutated!(block_cause = WorkflowBlockCause::ExplicitManifestBlock);
        mutated!(block_source_manifest_revision = None);
        mutated!(plan_lineage_reset_pending = true);
        mutated!(
            displayed_reset_reason_hash = Some(hash_displayed_reset_reason("displayed reason"))
        );
        for mutation in mutations {
            assert_ne!(
                baseline,
                decide_workflow_recovery(&mutation).source_state_fingerprint
            );
        }

        let mut rich = source.clone();
        rich.active_runs = vec![WorkflowRecoveryActiveRun {
            task_id: "active-task".into(),
            node_id: "task-1-impl".into(),
            status: DelegationRunStatus::Reserving,
            generation: 1,
            lineage_ordinal: 1,
            replaced_task_id: None,
        }];
        rich.frozen_task_cohorts = vec![WorkflowRecoveryFrozenTaskCohort {
            task_index: 1,
            implementer_node_id: "task-1-impl".into(),
            reviewer_node_ids: vec!["task-1-reviewer".into()],
            route_complete: true,
            unresolved: false,
            evidence_consistent: true,
        }];
        let rich_fingerprint = decide_workflow_recovery(&rich).source_state_fingerprint;
        macro_rules! assert_nested_mutation {
            ($mutation:expr) => {{
                let mut changed = rich.clone();
                ($mutation)(&mut changed);
                assert_ne!(
                    rich_fingerprint,
                    decide_workflow_recovery(&changed).source_state_fingerprint
                );
            }};
        }
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.design.as_mut().unwrap().rel_path = "docs/other-design.md".into()
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.design.as_mut().unwrap().digest = "sha256:other-design".into()
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.plan.as_mut().unwrap().rel_path = "docs/other-plan.md".into()
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.plan.as_mut().unwrap().digest = "sha256:other-plan".into()
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.active_plan_author.as_mut().unwrap().node_id = "other-author".into()
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.active_plan_author.as_mut().unwrap().work_unit_key = "other-key".into()
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.active_plan_author.as_mut().unwrap().agent_type = "other-agent".into()
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.active_plan_author.as_mut().unwrap().profile_id = Some("profile".into())
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.active_plan_author.as_mut().unwrap().active = false
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.active_plan_author.as_mut().unwrap().observed = false
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.active_plan_author.as_mut().unwrap().latest_task_id = Some("other-task".into())
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.active_plan_author.as_mut().unwrap().latest_status =
                Some(DelegationRunStatus::Failed)
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.active_plan_author.as_mut().unwrap().summary_validated = false
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.active_plan_author.as_mut().unwrap().artifact_digest = Some("other".into())
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.active_plan_author.as_mut().unwrap().gate_id = Some("gate".into())
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.active_plan_author.as_mut().unwrap().gate_cycle = Some(9)
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.active_plan_author.as_mut().unwrap().reviewed_task_id = Some("reviewed".into())
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value
                .active_plan_author
                .as_mut()
                .unwrap()
                .evidence_consistent = false
        });

        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.latest_plan_gate.as_mut().unwrap().gate_id = "other-gate".into()
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.latest_plan_gate.as_mut().unwrap().gate_cycle = 5
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.latest_plan_gate.as_mut().unwrap().outcome =
                GateSettlementOutcome::ChangesRequested
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.latest_plan_gate.as_mut().unwrap().content_fingerprint = "other".into()
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.latest_plan_gate.as_mut().unwrap().critical_count = 1
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.latest_plan_gate.as_mut().unwrap().important_count = 1
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.latest_plan_gate.as_mut().unwrap().minor_count = 1
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.latest_plan_gate.as_mut().unwrap().next_action =
                Some(PlanReviewNextAction::ContinueReview)
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value
                .latest_plan_gate
                .as_mut()
                .unwrap()
                .covered_author_task_id = Some("other-author-task".into())
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.latest_plan_gate.as_mut().unwrap().covered_plan_digest = Some("other-plan".into())
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value
                .latest_plan_gate
                .as_mut()
                .unwrap()
                .required_reviewer_node_ids = vec!["other-reviewer".into()]
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value
                .latest_plan_gate
                .as_mut()
                .unwrap()
                .reviewer_evidence_count = 2
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.latest_plan_gate.as_mut().unwrap().evidence_consistent = false
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value
                .latest_plan_gate
                .as_mut()
                .unwrap()
                .lineage_reset_consumed = true
        });

        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.binding_lifecycle[0].node_id = "other-node".into()
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.binding_lifecycle[0].work_unit_key = "other-work-unit".into()
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.binding_lifecycle[0].role = "reviewer".into()
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.binding_lifecycle[0].agent_type = "codex".into()
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.binding_lifecycle[0].profile_id = Some("profile".into())
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.binding_lifecycle[0].phase_id = "final".into()
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.binding_lifecycle[0].task_index = Some(2)
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.binding_lifecycle[0].introduced_revision = 2
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.binding_lifecycle[0].retired_revision = Some(8)
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.binding_lifecycle[0].observed = true
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.binding_lifecycle[0].retained_observed = true
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.binding_lifecycle[0].frozen = true
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.binding_lifecycle[0].node_outcome = Some("canceled".into())
        });

        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.active_runs[0].task_id = "other-active-task".into()
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.active_runs[0].node_id = "other-active-node".into()
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.active_runs[0].status = DelegationRunStatus::Running
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.active_runs[0].generation = 2
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.active_runs[0].lineage_ordinal = 2
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.active_runs[0].replaced_task_id = Some("replaced-task".into())
        });

        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.frozen_task_cohorts[0].task_index = 2
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.frozen_task_cohorts[0].implementer_node_id = "other-impl".into()
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.frozen_task_cohorts[0].reviewer_node_ids = vec!["other-reviewer".into()]
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.frozen_task_cohorts[0].route_complete = false
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.frozen_task_cohorts[0].unresolved = true
        });
        assert_nested_mutation!(|value: &mut WorkflowRecoverySnapshot| {
            value.frozen_task_cohorts[0].evidence_consistent = false
        });

        let excluded_before = (
            "Plan prose",
            "delegation prompt",
            "external-session-raw-1",
            "expanded-ui-section",
        );
        let excluded_after = (
            "different Plan prose",
            "different prompt",
            "external-session-raw-2",
            "collapsed-ui-section",
        );
        assert_ne!(excluded_before, excluded_after);
        let canonical_source = serde_json::to_string(&source).unwrap();
        for excluded in [
            excluded_before.0,
            excluded_before.1,
            excluded_before.2,
            excluded_before.3,
            excluded_after.0,
            excluded_after.1,
            excluded_after.2,
            excluded_after.3,
        ] {
            assert!(!canonical_source.contains(excluded));
        }
        assert_eq!(
            baseline,
            decide_workflow_recovery(&source).source_state_fingerprint
        );
    }
}
