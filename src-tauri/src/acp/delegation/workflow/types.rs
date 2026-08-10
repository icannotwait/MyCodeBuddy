//! Shared types for workflow key derivation and manifest validation.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::db::entities::delegation_workflow::CompletionProtocolMode;

use super::completion_intent::{
    CompletionIntent, CompletionIntentSource, CompletionOutcome, CompletionRole,
};

/// Max length of a canonical `work_unit_key` after normalization (MCP + A1).
/// Counted as Unicode scalar values (`str::chars`), not UTF-8 bytes.
pub const MAX_WORK_UNIT_KEY_LEN: usize = 200;

/// Canonical phase ids for brainstorm-to-delivery work units.
pub const PHASE_DESIGN: &str = "design";
pub const PHASE_PLAN: &str = "plan";
pub const PHASE_TASKS: &str = "tasks";
pub const PHASE_FINAL: &str = "final";

/// A15.2 concrete v1 bounds (validator + UI agree).
pub const MAX_TASKS: usize = 100;
pub const MAX_NODES: usize = 400;
pub const MAX_EDGES: usize = 800;
pub const MAX_GATES: usize = 50;
pub const MAX_ADJUDICATION_SUMMARY_BYTES: usize = 4 * 1024;
pub const MAX_MANIFEST_JSON_BYTES: usize = 512 * 1024;
pub const COMPLETE_WORK_SUMMARY_MAX_BYTES: usize = 4 * 1024;
pub const COMPLETE_WORK_REPORT_FILE_MAX_BYTES: usize = 1024;
pub const CURRENT_COMPLETION_PROTOCOL_VERSION: i64 = 2;
pub const COMPLETION_PROTOCOL_VERSION_V2: u32 = 2;
pub const EVIDENCE_SCOPE_SCHEMA_VERSION_V2: u32 = 2;

pub fn current_completion_protocol_mode() -> CompletionProtocolMode {
    CompletionProtocolMode::V2Enforce
}

pub fn reject_removed_completion_protocol_configuration(
) -> Result<(), super::error::CompletionProtocolConfigurationRemoved> {
    for variable in [
        "CODEG_COMPLETION_PROTOCOL_MODE",
        "CODEG_COMPLETION_PROTOCOL_OVERRIDES",
    ] {
        if std::env::var_os(variable).is_some() {
            return Err(super::error::CompletionProtocolConfigurationRemoved { variable });
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyWorkflowLink {
    pub workflow_id: String,
    pub conversation_id: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionProtocolWorkflowProjection {
    pub version: i64,
    pub mode: crate::db::entities::delegation_workflow::CompletionProtocolMode,
    pub creation_mode: crate::db::entities::delegation_workflow::CompletionProtocolMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_source: Option<LegacyWorkflowLink>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub v2_successor: Option<LegacyWorkflowLink>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only_reason: Option<String>,
    pub automatic_root_wake: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionScopeRole {
    DesignRoot,
    DesignReviewer,
    PlanAuthor,
    PlanReviewer,
    TaskImplementer,
    TaskReviewer,
    FinalFixer,
    FinalReviewer,
}

impl CompletionScopeRole {
    pub const fn completion_role(self) -> CompletionRole {
        match self {
            Self::DesignRoot
            | Self::DesignReviewer
            | Self::PlanReviewer
            | Self::TaskReviewer
            | Self::FinalReviewer => CompletionRole::Reviewer,
            Self::PlanAuthor => CompletionRole::Author,
            Self::TaskImplementer => CompletionRole::Implementer,
            Self::FinalFixer => CompletionRole::Fixer,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StableNodeIdentityV2 {
    pub node_id: String,
    pub role: CompletionRole,
    pub phase_id: String,
    pub task_index: Option<u32>,
    pub agent_type: String,
    pub profile_id: Option<String>,
    pub work_unit_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequirementsIdentityV1 {
    pub design_digest: String,
    pub design_rel_path: String,
    pub design_settlement_scope: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterialIdentitySummary {
    pub key: String,
    pub body_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstructionBlockV1 {
    pub template_id: String,
    pub template_version: u32,
    pub canonical_utf8: String,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ArtifactSubjectIdentityV2 {
    DocumentSha256 {
        rel_path: String,
        digest: String,
    },
    GitHeadV1 {
        digest: String,
    },
    PlanMaterial {
        plan_rel_path: String,
        gate_lineage: String,
        material_selector_digest: String,
        selected_material_digest: String,
    },
    PendingDocument {
        rel_path: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewedProducerIdentityV2 {
    pub task_id: String,
    pub generation: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceScopeInputV2 {
    pub completion_protocol_version: u32,
    pub scope_schema_version: u32,
    pub workflow_id: String,
    pub node: StableNodeIdentityV2,
    pub gate_id: Option<String>,
    pub gate_lineage: Option<String>,
    pub review_round: Option<u32>,
    pub artifact_subject: ArtifactSubjectIdentityV2,
    pub reviewed_producer: Option<ReviewedProducerIdentityV2>,
    pub instruction_block_digest: String,
    pub review_scope_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeNodePolicyV2 {
    pub node_id: String,
    pub role: CompletionRole,
    pub phase_id: String,
    pub task_index: Option<u32>,
    pub agent_type: String,
    pub profile_id: Option<String>,
    pub work_unit_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeEdgeV2 {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case", deny_unknown_fields)]
pub enum RoleReviewScopeV2 {
    DesignRoot {
        workflow_kind: String,
        design: DocumentRef,
        gate_lineage: String,
        policy_digest: String,
    },
    DesignReviewer {
        workflow_kind: String,
        design: DocumentRef,
        policy_digest: String,
    },
    PlanAuthor {
        plan_target_rel_path: String,
        requirements_identity: String,
    },
    PlanReviewer {
        requirements_identity: String,
        plan_subject: PlanSubjectIdentityV2,
        risk_policy_version: String,
        policy_digest: String,
    },
    TaskImplementer {
        task_specification_identity: String,
        dependency_identities: Vec<String>,
        route_digest: String,
        admitted_plan_identity: String,
    },
    TaskReviewer {
        task_specification_identity: String,
        risk_policy_digest: String,
        review_requirements_digest: String,
        admitted_plan_identity: String,
        reviewed_producer: ReviewedProducerIdentityV2,
    },
    FinalFixer {
        final_findings_identity: String,
        branch_tip: String,
    },
    FinalReviewer {
        active_plan_identity: String,
        ordered_task_output_identities: Vec<String>,
        final_review_requirements_digest: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CompletionArtifactV2 {
    DocumentSha256 { rel_path: String, digest: String },
    GitHeadV1 { head: String },
}

impl CompletionArtifactV2 {
    pub fn digest(&self) -> &str {
        match self {
            Self::DocumentSha256 { digest, .. } => digest,
            Self::GitHeadV1 { head } => head,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionEvidenceBindingV2 {
    pub workflow_id: String,
    pub task_id: String,
    pub node_id: String,
    pub role: CompletionRole,
    pub phase_id: String,
    pub task_index: Option<u32>,
    pub gate_id: Option<String>,
    pub gate_lineage: Option<String>,
    pub review_round: Option<u32>,
    pub reviewed_task_id: Option<String>,
    pub reviewed_generation: Option<i64>,
    pub manifest_revision_observed: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionEvidenceV2 {
    pub version: u32,
    pub intent: CompletionIntent,
    pub binding: CompletionEvidenceBindingV2,
    pub artifact: CompletionArtifactV2,
    pub review_scope_digest: String,
    pub evidence_scope_digest: String,
    pub captured_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceValidationContext {
    pub role: CompletionRole,
    pub binding: CompletionEvidenceBindingV2,
    pub artifact: CompletionArtifactV2,
    pub scope: EvidenceScopeInputV2,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedCompletionEvidence {
    pub evidence: CompletionEvidenceV2,
    pub evidence_validated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionCompletionContextV2 {
    pub scope_role: CompletionScopeRole,
    pub instruction: InstructionBlockV1,
    pub review_scope: RoleReviewScopeV2,
    pub review_scope_digest: String,
    pub evidence_scope: EvidenceScopeInputV2,
    pub evidence_scope_digest: String,
    pub material_selector_digest: Option<String>,
    pub subject_material_digest: Option<String>,
    pub requirements_identity: Option<String>,
    pub task_specification_identity: Option<String>,
    pub final_findings_identity: Option<String>,
    pub manifest_revision_observed: u64,
    pub graph_revision_observed: u64,
    pub required_reviewer_node_ids: Vec<String>,
    pub display_title: Option<String>,
    pub legacy_content_fingerprint: Option<String>,
}

impl CompletionIntentSource {
    pub const fn is_platform_supported(self) -> bool {
        matches!(
            self,
            Self::CompleteWork | Self::AssistantConclusion | Self::Report | Self::UserAdjudication
        )
    }
}

/// Semantic payload accepted from the workflow child. Identity and workflow
/// routing are always supplied by the platform transport/token binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompleteWorkRequest {
    pub outcome: CompletionOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_file: Option<String>,
}

/// Durable acknowledgement returned after a completion tool call is accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedToolIntent {
    pub intent_id: String,
    pub task_id: String,
    pub child_tool_call_id: String,
    pub accepted_ordinal: i64,
    pub outcome: CompletionOutcome,
    pub summary: Option<String>,
    pub report_file: Option<String>,
}

/// Immutable workflow identity passed from committed run admission to the
/// forced-child MCP launch. It is never populated from model arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowChildMcpBinding {
    pub task_id: String,
    pub workflow_id: String,
    pub protocol_version: i64,
    pub node_id: String,
}

/// Fixed workflow kind for this feature.
pub const WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY: &str = "brainstorm_to_delivery";

/// Supported schema version for `ManifestDocument`.
pub const MANIFEST_SCHEMA_VERSION: u32 = 2;

/// Exact Task risk policy accepted by manifest schema v2.
pub const TASK_RISK_POLICY_VERSION: &str = "b2d_task_risk_v1";

/// Typed errors for key derivation and manifest validation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorkflowError {
    #[error("work unit key exceeds {MAX_WORK_UNIT_KEY_LEN} characters")]
    KeyTooLong,
    #[error("invalid path: {0}")]
    InvalidPath(String),
    #[error("dependency cycle detected")]
    Cycle,
    #[error("duplicate id: {0}")]
    DuplicateId(String),
    #[error("bounds exceeded: {0}")]
    BoundsExceeded(String),
    #[error("role mismatch: {0}")]
    RoleMismatch(String),
    #[error("invalid schema version: expected {MANIFEST_SCHEMA_VERSION}, got {0}")]
    InvalidSchemaVersion(u32),
    #[error("unsupported workflow kind: {0}")]
    UnsupportedWorkflowKind(String),
    #[error("invalid gate shape: {0}")]
    InvalidGateShape(String),
    #[error("work unit key mismatch for node {node_id}: expected {expected}, got {got}")]
    KeyMismatch {
        node_id: String,
        expected: String,
        got: String,
    },
    #[error("missing field: {0}")]
    MissingField(String),
    #[error("unknown reference: {0}")]
    UnknownReference(String),
    #[error("invalid field: {0}")]
    InvalidField(String),
    #[error("invalid agent type: {0}")]
    InvalidAgentType(String),
    #[error("invalid task index: {0}")]
    InvalidTaskIndex(String),
    #[error(transparent)]
    RiskAssessmentInvalid(Box<WorkflowError>),
    #[error(transparent)]
    TaskRouteMismatch(Box<WorkflowError>),
}

/// Materials used to build a canonical A1 `work_unit_key`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkUnitKeyParts<'a> {
    Design {
        rel_doc_path: &'a str,
        agent_type: &'a str,
        profile_id: Option<&'a str>,
    },
    PlanAuthor {
        rel_plan_path: &'a str,
        agent_type: &'a str,
        profile_id: Option<&'a str>,
    },
    PlanReviewer {
        rel_plan_path: &'a str,
        agent_type: &'a str,
        profile_id: Option<&'a str>,
    },
    TaskImplementer {
        task_index: u32,
        agent_type: &'a str,
        profile_id: Option<&'a str>,
    },
    TaskReviewer {
        task_index: u32,
        agent_type: &'a str,
        profile_id: Option<&'a str>,
    },
    FinalReviewer {
        agent_type: &'a str,
        profile_id: Option<&'a str>,
    },
    FinalFixer {
        agent_type: &'a str,
        profile_id: Option<&'a str>,
    },
}

/// Result of parsing a recognized A1-grammar work unit key (A11).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedWorkUnitKey {
    Design {
        rel_doc_path: String,
        agent_type: String,
        profile_id: Option<String>,
    },
    PlanAuthor {
        rel_plan_path: String,
        agent_type: String,
        profile_id: Option<String>,
    },
    PlanReviewer {
        rel_plan_path: String,
        agent_type: String,
        profile_id: Option<String>,
    },
    TaskImplementer {
        task_index: u32,
        agent_type: String,
        profile_id: Option<String>,
    },
    TaskReviewer {
        task_index: u32,
        agent_type: String,
        profile_id: Option<String>,
    },
    FinalReviewer {
        agent_type: String,
        profile_id: Option<String>,
    },
    FinalFixer {
        agent_type: String,
        profile_id: Option<String>,
    },
}

/// Manifest publication / lifecycle state accepted on the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestWorkflowState {
    Skeleton,
    Estimated,
    Approved,
    Blocked,
}

/// Durable kind of an immutable manifest revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestRevisionKind {
    Publication,
    StateOnly,
}

impl ManifestRevisionKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Publication => "publication",
            Self::StateOnly => "state_only",
        }
    }

    pub fn from_db(value: Option<&str>) -> Result<Self, String> {
        match value {
            None | Some("publication") => Ok(Self::Publication),
            Some("state_only") => Ok(Self::StateOnly),
            Some(other) => Err(format!("unknown manifest revision kind: {other}")),
        }
    }
}

/// Durable reason that placed the active workflow projection in `blocked`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowBlockCause {
    PlanUserDecisionRequired,
    PlanGateBlocked,
    ExplicitManifestBlock,
    UnresolvedTaskCohort,
    DurableStateInconsistent,
    LegacyUnknown,
}

impl WorkflowBlockCause {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PlanUserDecisionRequired => "plan_user_decision_required",
            Self::PlanGateBlocked => "plan_gate_blocked",
            Self::ExplicitManifestBlock => "explicit_manifest_block",
            Self::UnresolvedTaskCohort => "unresolved_task_cohort",
            Self::DurableStateInconsistent => "durable_state_inconsistent",
            Self::LegacyUnknown => "legacy_unknown",
        }
    }

    pub fn from_db(value: Option<&str>) -> Result<Self, String> {
        match value {
            None => Ok(Self::LegacyUnknown),
            Some("plan_user_decision_required") => Ok(Self::PlanUserDecisionRequired),
            Some("plan_gate_blocked") => Ok(Self::PlanGateBlocked),
            Some("explicit_manifest_block") => Ok(Self::ExplicitManifestBlock),
            Some("unresolved_task_cohort") => Ok(Self::UnresolvedTaskCohort),
            Some("durable_state_inconsistent") => Ok(Self::DurableStateInconsistent),
            Some(other) => Err(format!("unknown workflow block cause: {other}")),
        }
    }

    pub fn from_transition_reason(reason: &str) -> Result<Self, String> {
        Self::from_db(Some(reason))
    }
}

/// Node kind on a published manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestNodeKind {
    Milestone,
    WorkUnit,
    Gate,
    Placeholder,
}

/// Work-unit role on a delegated node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestNodeRole {
    Author,
    Reviewer,
    Implementer,
    Fixer,
}

/// Derived risk level for one planned Task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskRiskLevel {
    Normal,
    High,
}

/// Any active hard trigger forces a high-risk Task route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskHardTriggerKind {
    ConcurrencyLifecycle,
    SecurityTrustBoundary,
    MigrationDestructivePersistence,
    PublicCompatibility,
    UnsafeFfi,
    UpdateRollback,
}

/// Weighted soft signals used when no hard trigger is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskSoftSignalKind {
    CrossRuntimeOrProcess,
    BroadProductionSurface,
    MultipleOwnershipModules,
    SharedInterface,
    DependencyOrBuild,
    MultiLayerWithoutTestSeam,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestTaskHardTrigger {
    pub kind: TaskHardTriggerKind,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestTaskSoftSignal {
    pub kind: TaskSoftSignalKind,
    pub score: u32,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestTaskRisk {
    pub level: TaskRiskLevel,
    pub hard_triggers: Vec<ManifestTaskHardTrigger>,
    pub soft_signals: Vec<ManifestTaskSoftSignal>,
    pub score: u32,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestTaskRoute {
    pub implementer_node_id: String,
    pub reviewer_node_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestTaskPolicy {
    pub task_index: u32,
    pub risk: ManifestTaskRisk,
    pub route: ManifestTaskRoute,
    #[serde(default)]
    pub allow_noop_verification: bool,
}

/// Optional durable node outcome (B14.3 cancel).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestNodeOutcome {
    Canceled,
}

/// Document-gate resolution mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionMode {
    /// Concurrent/multi-reviewer parent adjudication (default for Plan).
    ParentAdjudication,
    /// Zero-reviewer Design self-review (A12 only).
    SelfReview,
}

/// Document gate kind (Design / Plan only).
/// Optional on the wire; inferred fail-closed from reviewers when absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentGateKind {
    Design,
    Plan,
}

impl DocumentGateKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Design => PHASE_DESIGN,
            Self::Plan => PHASE_PLAN,
        }
    }

    pub const fn expected_reviewer_phase(self) -> &'static str {
        self.as_str()
    }
}

/// Workspace-relative document identity (path + digest).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentRef {
    pub rel_path: String,
    pub digest: String,
}

/// Exact canonical Task specification identity payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSpecificationIdentityV1 {
    pub schema: String,
    pub task_index: u32,
    pub body_sha256: String,
}

/// Freshness identity for one Plan Reviewer's selected material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanSubjectIdentityV2 {
    pub plan_rel_path: String,
    pub gate_lineage: String,
    pub material_selector_digest: String,
    pub selected_material_digest: String,
}

/// Platform-authored proof for a same-lineage localized Plan correction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanLocalizedChangeV2 {
    pub schema: String,
    pub prior_plan_digest: String,
    pub current_plan_digest: String,
    pub changed_keys: BTreeSet<String>,
    pub classifier_version: String,
    pub authorization_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanLineageResetReason {
    UnparseableMaterial,
    AmbiguousKeySet,
    SharedMaterialChanged,
    PolicyOrRouteChanged,
    MissingAuthorization,
    SelectorMismatch,
    UncoveredChange,
}

/// Conservative result of comparing two complete Plan material snapshots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlanChangeClassification {
    Localized {
        change: PlanLocalizedChangeV2,
        corrective_reviewer_node_ids: BTreeSet<String>,
    },
    NewLineage {
        changed_keys: BTreeSet<String>,
        reason: PlanLineageResetReason,
        reviewer_cohort_node_ids: BTreeSet<String>,
    },
}

/// Phase entry in a manifest document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestPhase {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Node entry in a manifest document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestNode {
    pub id: String,
    pub kind: ManifestNodeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<ManifestNodeRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_unit_key: Option<String>,
    /// Required on the wire (no serde default). Empty list is allowed.
    pub deps: Vec<String>,
    /// Document reviewers default `true` when the field is present as null/absent
    /// only via Option; callers constructing in Rust should set explicitly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_outcome: Option<ManifestNodeOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Directed edge between stable node ids.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEdge {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub from: String,
    pub to: String,
}

/// Document gate definition (Design / Plan).
///
/// Frozen wire fields: `id`, `required_reviewer_node_ids`, `resolution_mode`.
/// `gate_kind` is optional; when absent the validator infers it fail-closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestGate {
    pub id: String,
    /// Complete configured reviewer group for this document gate.
    pub reviewer_cohort_node_ids: Vec<String>,
    /// Required on the wire (no serde default). Empty only for Design self_review.
    pub required_reviewer_node_ids: Vec<String>,
    pub resolution_mode: ResolutionMode,
    /// Optional `design` | `plan`. Omitted → inferred from reviewer set / mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_kind: Option<DocumentGateKind>,
}

/// Raw published manifest document (Task 2 freeze).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestDocument {
    pub schema_version: u32,
    pub workflow_kind: String,
    pub plan_target_rel_path: String,
    pub risk_policy_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_manifest_revision: Option<u64>,
    pub publication_token: String,
    pub workflow_state: ManifestWorkflowState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub design: Option<DocumentRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<DocumentRef>,
    /// Required on the wire (no serde default).
    pub phases: Vec<ManifestPhase>,
    /// Required on the wire (no serde default).
    pub nodes: Vec<ManifestNode>,
    /// Required on the wire (no serde default).
    pub edges: Vec<ManifestEdge>,
    /// Required on the wire (no serde default).
    pub gates: Vec<ManifestGate>,
    /// Required on the wire (no serde default).
    pub task_policies: Vec<ManifestTaskPolicy>,
}

/// Normalized, validated work-unit node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedNode {
    pub id: String,
    pub kind: ManifestNodeKind,
    pub phase_id: Option<String>,
    pub role: Option<ManifestNodeRole>,
    pub agent_type: Option<String>,
    pub profile_id: Option<String>,
    pub task_index: Option<u32>,
    pub work_unit_key: Option<String>,
    pub deps: Vec<String>,
    pub required: bool,
    pub node_outcome: Option<ManifestNodeOutcome>,
    pub title: Option<String>,
}

/// Normalized, validated gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedGate {
    pub id: String,
    pub reviewer_cohort_node_ids: Vec<String>,
    pub required_reviewer_node_ids: Vec<String>,
    pub resolution_mode: ResolutionMode,
    pub gate_kind: DocumentGateKind,
}

/// Output of `validate_manifest_document`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedManifest {
    pub schema_version: u32,
    pub workflow_kind: String,
    pub plan_target_rel_path: String,
    pub risk_policy_version: String,
    pub workflow_id: Option<String>,
    pub expected_manifest_revision: Option<u64>,
    pub publication_token: String,
    pub workflow_state: ManifestWorkflowState,
    pub design: Option<DocumentRef>,
    pub plan: Option<DocumentRef>,
    pub phases: Vec<ManifestPhase>,
    pub nodes: Vec<NormalizedNode>,
    pub edges: Vec<ManifestEdge>,
    pub gates: Vec<NormalizedGate>,
    pub task_policies: Vec<ManifestTaskPolicy>,
    /// Distinct Task indices present on work units (1-based).
    pub task_count: usize,
}

#[cfg(test)]
mod tests {
    #[test]
    fn removed_completion_protocol_environment_rejects_every_historical_value() {
        for variable in [
            "CODEG_COMPLETION_PROTOCOL_MODE",
            "CODEG_COMPLETION_PROTOCOL_OVERRIDES",
        ] {
            for value in ["v1", "v2_shadow", "v2_enforce"] {
                let other = if variable == "CODEG_COMPLETION_PROTOCOL_MODE" {
                    "CODEG_COMPLETION_PROTOCOL_OVERRIDES"
                } else {
                    "CODEG_COMPLETION_PROTOCOL_MODE"
                };
                temp_env::with_vars([(variable, Some(value)), (other, None::<&str>)], || {
                    let error = super::reject_removed_completion_protocol_configuration()
                        .expect_err("removed configuration must fail startup");
                    assert_eq!(error.code(), "completion_protocol_configuration_removed");
                    assert_eq!(error.variable, variable);
                });
            }
        }
    }
}
