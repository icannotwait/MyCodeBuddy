//! Canonical, domain-separated completion evidence scope.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder};
use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde::ser::{
    SerializeMap, SerializeSeq, SerializeStruct, SerializeStructVariant, SerializeTuple,
    SerializeTupleStruct, SerializeTupleVariant,
};
use serde::Serialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;
use unicode_normalization::UnicodeNormalization;

use super::final_findings::{verify_final_findings_package_model_v1, FinalFindingsPackageV1};
use super::plan_material::{
    bind_plan_material, derive_plan_reviewer_selector, parse_plan_material, BoundPlanMaterialMap,
    MaterialSelectorV1, PlanMaterialMap, MAX_PLAN_MATERIAL_BYTES,
};
use super::store::load_active_manifest_snapshot;
use super::types::{
    AdmissionCompletionContextV2, ArtifactSubjectIdentityV2, CompletionArtifactV2,
    CompletionEvidenceBindingV2, CompletionEvidenceV2, CompletionScopeRole, DocumentGateKind,
    EvidenceScopeInputV2, EvidenceValidationContext, InstructionBlockV1, MaterialIdentitySummary,
    NormalizedManifest, PlanSubjectIdentityV2, RequirementsIdentityV1, ReviewedProducerIdentityV2,
    RoleReviewScopeV2, ScopeEdgeV2, StableNodeIdentityV2, TaskSpecificationIdentityV1,
    ValidatedCompletionEvidence, COMPLETE_WORK_SUMMARY_MAX_BYTES, COMPLETION_PROTOCOL_VERSION_V2,
    EVIDENCE_SCOPE_SCHEMA_VERSION_V2, PHASE_DESIGN, PHASE_FINAL, PHASE_PLAN, PHASE_TASKS,
};
use super::{build_conclusion_suffix, normalize_rel_path, CompletionRole};
use crate::db::entities::delegation_final_findings_package::FinalFindingsPackageStatus;
use crate::db::entities::delegation_workflow_gate_settlement::GateSettlementOutcome;
use crate::db::entities::{
    delegation_final_findings_package, delegation_task_run, delegation_workflow,
    delegation_workflow_gate_settlement, delegation_workflow_gate_state,
    delegation_workflow_node_binding, delegation_workflow_run_binding,
};

const DIGEST_SCHEMA_VERSION: u32 = 1;
const MAX_CANONICAL_JSON_BYTES: usize = 512 * 1024;
const MAX_INSTRUCTION_BLOCK_BYTES: usize = 64 * 1024;
const MAX_FINAL_FIXER_INSTRUCTION_BYTES: usize = 4 * 1024 * 1024;
const MAX_EVIDENCE_JSON_BYTES: usize = 64 * 1024;
const MAX_DESIGN_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;
const INSTRUCTION_TEMPLATE_ID: &str = "workflow_completion";
const INSTRUCTION_TEMPLATE_VERSION: u32 = 1;
const INSTRUCTION_DOMAIN: &str = "codeg.completion.instruction.v1";
const REVIEW_SCOPE_DOMAIN: &str = "codeg.completion.review_scope.v2";
const EVIDENCE_SCOPE_DOMAIN: &str = "codeg.completion.evidence_scope.v2";

const ALLOWED_DIGEST_DOMAINS: &[&str] = &[
    "codeg.completion.requirements.v1",
    INSTRUCTION_DOMAIN,
    "codeg.completion.task_specification.v1",
    "codeg.completion.material_selector.v1",
    "codeg.completion.selected_material.v1",
    "codeg.completion.plan_subject.v2",
    "codeg.completion.plan_change.v2",
    "codeg.completion.final_findings.v1",
    "codeg.completion.policy.v2",
    "codeg.completion.route.v2",
    "codeg.completion.dependencies.v2",
    "codeg.completion.plan_identity.v2",
    REVIEW_SCOPE_DOMAIN,
    EVIDENCE_SCOPE_DOMAIN,
];

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EvidenceScopeError {
    #[error("canonical completion JSON is invalid: {0}")]
    InvalidCanonicalJson(String),
    #[error("unsupported completion digest domain or schema")]
    UnsupportedDomain,
    #[error("completion Plan material is invalid: {0}")]
    PlanMaterialInvalid(String),
    #[error("completion instruction binding failed: {0}")]
    InstructionBindingFailed(String),
    #[error("completion evidence is corrupt: {0}")]
    EvidenceCorrupt(String),
    #[error("completion outcome is incompatible with its durable role")]
    OutcomeRoleMismatch,
    #[error("completion scope changed")]
    ScopeChanged,
}

impl EvidenceScopeError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidCanonicalJson(_) | Self::UnsupportedDomain => {
                "completion_evidence_corrupt"
            }
            Self::PlanMaterialInvalid(_) => "completion_plan_material_invalid",
            Self::InstructionBindingFailed(_) => "completion_instruction_binding_failed",
            Self::EvidenceCorrupt(_) => "completion_evidence_corrupt",
            Self::OutcomeRoleMismatch => "completion_outcome_role_mismatch",
            Self::ScopeChanged => "completion_scope_changed",
        }
    }
}

pub struct InstructionBlockInput<'a> {
    pub role: CompletionRole,
    pub phase_id: &'a str,
    pub task_index: Option<u32>,
    pub gate_id: Option<&'a str>,
    pub review_round: Option<u32>,
    pub material_identities: &'a [MaterialIdentitySummary],
}

pub struct DesignRootScopeInput<'a> {
    pub workflow_kind: &'a str,
    pub design: &'a super::types::DocumentRef,
    pub gate_id: &'a str,
    pub gate_lineage: &'a str,
    pub resolution_mode: super::types::ResolutionMode,
}

pub struct WorkflowStore<'a, C: ConnectionTrait> {
    pub conn: &'a C,
    pub workspace_root: &'a Path,
}

impl<'a, C: ConnectionTrait> WorkflowStore<'a, C> {
    pub fn new(conn: &'a C, workspace_root: &'a Path) -> Self {
        Self {
            conn,
            workspace_root,
        }
    }
}

pub struct AdmissionCandidate<'a> {
    pub workflow: &'a delegation_workflow::Model,
    pub node: &'a delegation_workflow_node_binding::Model,
    pub task_id: &'a str,
    pub artifact_digest: Option<&'a str>,
    pub reviewed_task_id: Option<&'a str>,
    pub reviewed_generation: Option<i64>,
    pub producer_baseline_head: Option<&'a str>,
}

pub fn build_design_root_review_scope(
    input: &DesignRootScopeInput<'_>,
) -> Result<RoleReviewScopeV2, EvidenceScopeError> {
    if input.workflow_kind.trim().is_empty() || input.gate_id.trim().is_empty() {
        return Err(EvidenceScopeError::InstructionBindingFailed(
            "Design Root requires durable workflow and gate identities".into(),
        ));
    }
    if input.resolution_mode != super::types::ResolutionMode::SelfReview {
        return Err(EvidenceScopeError::InstructionBindingFailed(
            "Design Root requires the durable self_review policy".into(),
        ));
    }
    validate_sha256_token(input.gate_lineage, false)?;
    let policy_digest = policy_digest(&json!({
        "gate_id": input.gate_id,
        "resolution_mode": input.resolution_mode,
    }))?;
    let scope = RoleReviewScopeV2::DesignRoot {
        workflow_kind: input.workflow_kind.to_string(),
        design: input.design.clone(),
        gate_lineage: input.gate_lineage.to_string(),
        policy_digest,
    };
    canonical_json_bytes(&scope)?;
    Ok(scope)
}

pub async fn build_admission_completion_context<C: ConnectionTrait>(
    store: &WorkflowStore<'_, C>,
    candidate: &AdmissionCandidate<'_>,
) -> Result<AdmissionCompletionContextV2, EvidenceScopeError> {
    build_completion_context(store, candidate, GateRoundSelection::CurrentAdmission).await
}

pub(crate) async fn build_persisted_completion_context<C: ConnectionTrait>(
    store: &WorkflowStore<'_, C>,
    candidate: &AdmissionCandidate<'_>,
    binding: &delegation_workflow_run_binding::Model,
) -> Result<AdmissionCompletionContextV2, EvidenceScopeError> {
    build_completion_context(
        store,
        candidate,
        GateRoundSelection::PersistedEvidence {
            gate_lineage: binding.gate_lineage.as_deref(),
            review_round: binding.review_round,
        },
    )
    .await
}

#[derive(Clone, Copy)]
enum GateRoundSelection<'a> {
    CurrentAdmission,
    PersistedEvidence {
        gate_lineage: Option<&'a str>,
        review_round: Option<i64>,
    },
}

async fn build_completion_context<C: ConnectionTrait>(
    store: &WorkflowStore<'_, C>,
    candidate: &AdmissionCandidate<'_>,
    gate_round_selection: GateRoundSelection<'_>,
) -> Result<AdmissionCompletionContextV2, EvidenceScopeError> {
    if candidate.workflow.completion_protocol_version != i64::from(COMPLETION_PROTOCOL_VERSION_V2) {
        return Err(EvidenceScopeError::InstructionBindingFailed(
            "unsupported completion protocol version".into(),
        ));
    }
    let snapshot = load_active_manifest_snapshot(store.conn, candidate.workflow)
        .await
        .map_err(|error| EvidenceScopeError::InstructionBindingFailed(error.to_string()))?;
    let scope_role = completion_scope_role(candidate.node)?;
    let role = scope_role.completion_role();
    let task_index = candidate
        .node
        .task_index
        .map(|value| {
            u32::try_from(value).map_err(|_| {
                EvidenceScopeError::InstructionBindingFailed("invalid task index".into())
            })
        })
        .transpose()?;
    let node = StableNodeIdentityV2 {
        node_id: candidate.node.node_id.clone(),
        role,
        phase_id: candidate.node.phase_id.clone(),
        task_index,
        agent_type: candidate.node.agent_type.clone(),
        profile_id: candidate.node.profile_id.clone(),
        work_unit_key: candidate.node.work_unit_key.clone(),
    };
    let gate = load_admitted_gate_state(
        store,
        &snapshot.normalized,
        candidate.node,
        scope_role,
        gate_round_selection,
    )
    .await?;
    let gate_id = gate.as_ref().map(|value| value.gate_id.clone());
    let gate_lineage = gate.as_ref().map(|value| value.gate_lineage.clone());
    let review_round = gate.as_ref().and_then(|value| value.review_round);
    let required_reviewer_node_ids = gate
        .as_ref()
        .map_or_else(Vec::new, |value| value.required_reviewer_node_ids.clone());

    let reviewed_producer = match (candidate.reviewed_task_id, candidate.reviewed_generation) {
        (Some(task_id), Some(generation)) if generation >= 0 => Some(ReviewedProducerIdentityV2 {
            task_id: task_id.to_string(),
            generation,
        }),
        (None, None) => None,
        _ => {
            return Err(EvidenceScopeError::InstructionBindingFailed(
                "reviewed producer identity is incomplete".into(),
            ))
        }
    };

    let requirements =
        build_requirements_identity(store, &snapshot.normalized, &candidate.workflow.workflow_id)
            .await?;
    let requirements_identity = requirements.as_ref().map(|(_, digest)| digest.clone());
    let mut material_identities = Vec::new();
    let mut material_selector_digest = None;
    let mut subject_material_digest = None;
    let mut task_specification_identity = None;
    let mut final_findings_identity = None;
    let mut final_findings_package = None;

    let (review_scope, artifact_subject) = match scope_role {
        CompletionScopeRole::DesignRoot => {
            return Err(EvidenceScopeError::InstructionBindingFailed(
                "Design Root does not use delegated admission".into(),
            ))
        }
        CompletionScopeRole::DesignReviewer => {
            let design = verified_document(
                store.workspace_root,
                snapshot.normalized.design.as_ref().ok_or_else(|| {
                    EvidenceScopeError::InstructionBindingFailed(
                        "Design Reviewer requires a durable Design document".into(),
                    )
                })?,
                "Design",
                MAX_DESIGN_DOCUMENT_BYTES,
            )
            .await?;
            material_identities.push(MaterialIdentitySummary {
                key: "design".into(),
                body_sha256: design.digest.clone(),
            });
            let policy_digest = policy_digest(&json!({
                "gate_id": gate_id,
                "node": &node,
                "resolution_mode": gate
                    .as_ref()
                    .and_then(|value| value.resolution_mode),
            }))?;
            (
                RoleReviewScopeV2::DesignReviewer {
                    workflow_kind: candidate.workflow.workflow_kind.clone(),
                    design: design.clone(),
                    policy_digest,
                },
                ArtifactSubjectIdentityV2::DocumentSha256 {
                    rel_path: design.rel_path,
                    digest: design.digest,
                },
            )
        }
        CompletionScopeRole::PlanAuthor => {
            let requirements_identity = require_identity(
                requirements_identity.as_deref(),
                "Plan Author requirements identity",
            )?;
            material_identities.push(MaterialIdentitySummary {
                key: "requirements".into(),
                body_sha256: requirements_identity.clone(),
            });
            let plan_target_rel_path =
                normalize_rel_path(&snapshot.normalized.plan_target_rel_path)
                    .map_err(|error| EvidenceScopeError::PlanMaterialInvalid(error.to_string()))?;
            (
                RoleReviewScopeV2::PlanAuthor {
                    plan_target_rel_path: plan_target_rel_path.clone(),
                    requirements_identity,
                },
                ArtifactSubjectIdentityV2::PendingDocument {
                    rel_path: plan_target_rel_path,
                },
            )
        }
        CompletionScopeRole::PlanReviewer => {
            let requirements_identity = require_identity(
                requirements_identity.as_deref(),
                "Plan Reviewer requirements identity",
            )?;
            let (plan, bound) = load_bound_plan_material(store, &snapshot.normalized).await?;
            let selector = derive_plan_reviewer_selector(
                &snapshot.normalized,
                &candidate.node.node_id,
                &bound,
            )
            .map_err(|error| EvidenceScopeError::PlanMaterialInvalid(error.to_string()))?;
            material_identities = material_identity_summaries(bound.material(), &selector)?;
            let selector_digest = selector.digest();
            let selected_digest = canonical_json_sha256(
                "codeg.completion.selected_material.v1",
                DIGEST_SCHEMA_VERSION,
                &material_identities,
            )?;
            let lineage = gate_lineage.clone().ok_or_else(|| {
                EvidenceScopeError::PlanMaterialInvalid(
                    "Plan Reviewer has no durable gate lineage".into(),
                )
            })?;
            let plan_subject = PlanSubjectIdentityV2 {
                plan_rel_path: plan.rel_path.clone(),
                gate_lineage: lineage.clone(),
                material_selector_digest: selector_digest.clone(),
                selected_material_digest: selected_digest.clone(),
            };
            let policy_digest = plan_reviewer_policy_digest(
                &snapshot.normalized,
                candidate.node,
                gate_id.as_deref(),
            )?;
            material_selector_digest = Some(selector_digest.clone());
            subject_material_digest = Some(selected_digest.clone());
            (
                RoleReviewScopeV2::PlanReviewer {
                    requirements_identity,
                    plan_subject,
                    risk_policy_version: snapshot.normalized.risk_policy_version.clone(),
                    policy_digest,
                },
                ArtifactSubjectIdentityV2::PlanMaterial {
                    plan_rel_path: plan.rel_path,
                    gate_lineage: lineage,
                    material_selector_digest: selector_digest,
                    selected_material_digest: selected_digest,
                },
            )
        }
        CompletionScopeRole::TaskImplementer | CompletionScopeRole::TaskReviewer => {
            let task_index = task_index.ok_or_else(|| {
                EvidenceScopeError::PlanMaterialInvalid(
                    "Task completion scope requires task_index".into(),
                )
            })?;
            let (_, bound) = load_bound_plan_material(store, &snapshot.normalized).await?;
            let task_identity = bound
                .material()
                .task_identity(task_index)
                .map_err(|error| EvidenceScopeError::PlanMaterialInvalid(error.to_string()))?;
            let task_identity_digest = task_specification_digest(&task_identity)?;
            task_specification_identity = Some(task_identity_digest.clone());
            material_identities.push(MaterialIdentitySummary {
                key: format!("task.{task_index}"),
                body_sha256: task_identity.body_sha256.clone(),
            });
            let admitted_plan_identity =
                active_plan_identity(store, &snapshot.normalized, candidate.workflow).await?;
            let route_digest = task_route_digest(&snapshot.normalized, task_index)?;
            let artifact = require_git_subject(candidate, scope_role)?;
            if scope_role == CompletionScopeRole::TaskImplementer {
                (
                    RoleReviewScopeV2::TaskImplementer {
                        task_specification_identity: task_identity_digest,
                        dependency_identities: task_dependency_identities(
                            &snapshot.normalized,
                            &candidate.node.node_id,
                        )?,
                        route_digest,
                        admitted_plan_identity,
                    },
                    artifact,
                )
            } else {
                let reviewed_producer = reviewed_producer.clone().ok_or_else(|| {
                    EvidenceScopeError::InstructionBindingFailed(
                        "Task Reviewer requires a reviewed producer".into(),
                    )
                })?;
                (
                    RoleReviewScopeV2::TaskReviewer {
                        task_specification_identity: task_identity_digest,
                        risk_policy_digest: policy_digest(
                            &task_policy(&snapshot.normalized, task_index)?.risk,
                        )?,
                        review_requirements_digest: policy_digest(&json!({
                            "reviewer_node_id": candidate.node.node_id,
                            "route_digest": route_digest,
                        }))?,
                        admitted_plan_identity,
                        reviewed_producer,
                    },
                    artifact,
                )
            }
        }
        CompletionScopeRole::FinalFixer => {
            let final_gate = gate.as_ref().ok_or_else(|| {
                EvidenceScopeError::InstructionBindingFailed(
                    "Final Fixer requires a durable Final gate lineage".into(),
                )
            })?;
            let package =
                active_final_findings_package(store, candidate.workflow, final_gate).await?;
            let final_identity = package.final_findings_identity().to_owned();
            let branch_tip = candidate.producer_baseline_head.ok_or_else(|| {
                EvidenceScopeError::InstructionBindingFailed(
                    "Final Fixer requires a durable admission branch tip".into(),
                )
            })?;
            validate_digest_or_git_token("branch_tip", branch_tip)?;
            final_findings_identity = Some(final_identity.clone());
            final_findings_package = Some(package);
            material_identities.extend([
                MaterialIdentitySummary {
                    key: "final.branch_tip".into(),
                    body_sha256: git_identity_digest(branch_tip)?,
                },
                MaterialIdentitySummary {
                    key: "final.findings".into(),
                    body_sha256: final_identity.clone(),
                },
            ]);
            (
                RoleReviewScopeV2::FinalFixer {
                    final_findings_identity: final_identity,
                    branch_tip: branch_tip.to_string(),
                },
                ArtifactSubjectIdentityV2::GitHeadV1 {
                    digest: branch_tip.to_string(),
                },
            )
        }
        CompletionScopeRole::FinalReviewer => {
            let active_plan_identity =
                active_plan_identity(store, &snapshot.normalized, candidate.workflow).await?;
            let ordered_task_output_identities =
                ordered_task_output_identities(store, &snapshot.normalized, candidate.workflow)
                    .await?;
            let final_review_requirements_digest =
                final_review_requirements_digest(&snapshot.normalized, &candidate.node.node_id)?;
            material_identities.push(MaterialIdentitySummary {
                key: "final.plan".into(),
                body_sha256: active_plan_identity.clone(),
            });
            (
                RoleReviewScopeV2::FinalReviewer {
                    active_plan_identity,
                    ordered_task_output_identities,
                    final_review_requirements_digest,
                },
                require_git_subject(candidate, scope_role)?,
            )
        }
    };

    material_identities.sort_by(|left, right| left.key.cmp(&right.key));
    let instruction_input = InstructionBlockInput {
        role,
        phase_id: &candidate.node.phase_id,
        task_index,
        gate_id: gate_id.as_deref(),
        review_round,
        material_identities: &material_identities,
    };
    let instruction = if let Some(package) = final_findings_package.as_ref() {
        build_final_fixer_instruction_block(&instruction_input, package)?
    } else {
        build_instruction_block(&instruction_input)?
    };
    let review_scope_digest = review_scope_digest(&review_scope, &instruction)?;
    let evidence_scope = EvidenceScopeInputV2 {
        completion_protocol_version: COMPLETION_PROTOCOL_VERSION_V2,
        scope_schema_version: EVIDENCE_SCOPE_SCHEMA_VERSION_V2,
        workflow_id: candidate.workflow.workflow_id.clone(),
        node,
        gate_id,
        gate_lineage,
        review_round,
        artifact_subject,
        reviewed_producer,
        instruction_block_digest: instruction.digest.clone(),
        review_scope_digest: review_scope_digest.clone(),
    };
    let evidence_scope_digest = evidence_scope_digest(&evidence_scope)?;
    Ok(AdmissionCompletionContextV2 {
        scope_role,
        instruction,
        review_scope,
        review_scope_digest,
        evidence_scope,
        evidence_scope_digest,
        material_selector_digest,
        subject_material_digest,
        requirements_identity,
        task_specification_identity,
        final_findings_identity,
        manifest_revision_observed: u64::try_from(candidate.workflow.active_manifest_revision)
            .map_err(|_| {
                EvidenceScopeError::InstructionBindingFailed("negative manifest revision".into())
            })?,
        graph_revision_observed: u64::try_from(candidate.workflow.graph_revision).map_err(
            |_| EvidenceScopeError::InstructionBindingFailed("negative graph revision".into()),
        )?,
        required_reviewer_node_ids,
        display_title: snapshot
            .normalized
            .nodes
            .iter()
            .find(|node| node.id == candidate.node.node_id)
            .and_then(|node| node.title.clone()),
        legacy_content_fingerprint: None,
    })
}

#[derive(Debug)]
struct AdmittedGateState {
    gate_id: String,
    gate_lineage: String,
    review_round: Option<u32>,
    required_reviewer_node_ids: Vec<String>,
    resolution_mode: Option<super::types::ResolutionMode>,
}

fn completion_scope_role(
    node: &delegation_workflow_node_binding::Model,
) -> Result<CompletionScopeRole, EvidenceScopeError> {
    match (node.phase_id.as_str(), node.role.as_str()) {
        (PHASE_DESIGN, "reviewer") => Ok(CompletionScopeRole::DesignReviewer),
        (PHASE_PLAN, "author") => Ok(CompletionScopeRole::PlanAuthor),
        (PHASE_PLAN, "reviewer") => Ok(CompletionScopeRole::PlanReviewer),
        (PHASE_TASKS, "implementer") => Ok(CompletionScopeRole::TaskImplementer),
        (PHASE_TASKS, "reviewer") => Ok(CompletionScopeRole::TaskReviewer),
        (PHASE_FINAL, "fixer") => Ok(CompletionScopeRole::FinalFixer),
        (PHASE_FINAL, "reviewer") => Ok(CompletionScopeRole::FinalReviewer),
        _ => Err(EvidenceScopeError::InstructionBindingFailed(format!(
            "unsupported completion node role {}/{}",
            node.phase_id, node.role
        ))),
    }
}

async fn load_admitted_gate_state<C: ConnectionTrait>(
    store: &WorkflowStore<'_, C>,
    manifest: &NormalizedManifest,
    node: &delegation_workflow_node_binding::Model,
    scope_role: CompletionScopeRole,
    gate_round_selection: GateRoundSelection<'_>,
) -> Result<Option<AdmittedGateState>, EvidenceScopeError> {
    if matches!(
        scope_role,
        CompletionScopeRole::FinalFixer | CompletionScopeRole::FinalReviewer
    ) {
        let state = delegation_workflow_gate_state::Entity::find_by_id((
            node.workflow_id.clone(),
            "final".to_string(),
        ))
        .one(store.conn)
        .await
        .map_err(|error| EvidenceScopeError::InstructionBindingFailed(error.to_string()))?
        .ok_or_else(|| {
            EvidenceScopeError::InstructionBindingFailed(
                "Final admission requires a durable Final gate lineage".into(),
            )
        })?;
        validate_sha256_token(&state.gate_lineage, false)?;
        let review_round = if scope_role == CompletionScopeRole::FinalReviewer {
            let selected: BTreeSet<String> = serde_json::from_str(&state.selected_node_ids_json)
                .map_err(|error| EvidenceScopeError::InstructionBindingFailed(error.to_string()))?;
            if !selected.contains(&node.node_id) || state.current_review_round <= 0 {
                return Err(EvidenceScopeError::InstructionBindingFailed(format!(
                    "Final Reviewer {} is not selected for round {}",
                    node.node_id, state.current_review_round
                )));
            }
            Some(u32::try_from(state.current_review_round).map_err(|_| {
                EvidenceScopeError::InstructionBindingFailed(
                    "Final review round exceeds u32".into(),
                )
            })?)
        } else {
            None
        };
        return Ok(Some(AdmittedGateState {
            gate_id: "final".into(),
            gate_lineage: state.gate_lineage,
            review_round,
            required_reviewer_node_ids: Vec::new(),
            resolution_mode: None,
        }));
    }

    let gate_kind = match scope_role {
        CompletionScopeRole::DesignReviewer => Some(DocumentGateKind::Design),
        CompletionScopeRole::PlanReviewer => Some(DocumentGateKind::Plan),
        _ => None,
    };
    let Some(gate_kind) = gate_kind else {
        return Ok(None);
    };
    let gate = manifest
        .gates
        .iter()
        .find(|gate| {
            gate.gate_kind == gate_kind && gate.reviewer_cohort_node_ids.contains(&node.node_id)
        })
        .ok_or_else(|| {
            EvidenceScopeError::PlanMaterialInvalid(format!(
                "{} Reviewer is outside its durable gate cohort",
                gate_kind.as_str()
            ))
        })?;
    let state = delegation_workflow_gate_state::Entity::find_by_id((
        node.workflow_id.clone(),
        gate.id.clone(),
    ))
    .one(store.conn)
    .await
    .map_err(|error| EvidenceScopeError::InstructionBindingFailed(error.to_string()))?
    .ok_or_else(|| {
        EvidenceScopeError::PlanMaterialInvalid(format!(
            "gate {} has no durable lineage state",
            gate.id
        ))
    })?;
    validate_sha256_token(&state.gate_lineage, false)?;
    let selected: BTreeSet<String> = serde_json::from_str(&state.selected_node_ids_json)
        .map_err(|error| EvidenceScopeError::PlanMaterialInvalid(error.to_string()))?;
    let review_round = if selected.contains(&node.node_id) && state.current_review_round > 0 {
        state.current_review_round
    } else if scope_role == CompletionScopeRole::PlanReviewer {
        match gate_round_selection {
            GateRoundSelection::PersistedEvidence {
                gate_lineage: Some(persisted_lineage),
                review_round: Some(persisted_round),
            } if persisted_lineage == state.gate_lineage
                && persisted_round > 0
                && persisted_round < state.current_review_round =>
            {
                let localized_change_proof = delegation_workflow_gate_settlement::Entity::find()
                    .filter(
                        delegation_workflow_gate_settlement::Column::WorkflowId
                            .eq(node.workflow_id.clone()),
                    )
                    .filter(
                        delegation_workflow_gate_settlement::Column::GateLineage
                            .eq(state.gate_lineage.clone()),
                    )
                    .filter(delegation_workflow_gate_settlement::Column::GateId.eq(gate.id.clone()))
                    .filter(
                        delegation_workflow_gate_settlement::Column::LocalizedChangeDigest
                            .is_not_null(),
                    )
                    .filter(
                        delegation_workflow_gate_settlement::Column::ReviewRound
                            .gt(persisted_round),
                    )
                    .one(store.conn)
                    .await
                    .map_err(|error| {
                        EvidenceScopeError::InstructionBindingFailed(error.to_string())
                    })?;
                if localized_change_proof.is_none() {
                    return Err(EvidenceScopeError::PlanMaterialInvalid(format!(
                        "unselected reviewer {} has no localized-change proof for current lineage",
                        node.node_id
                    )));
                }
                persisted_round
            }
            _ => {
                return Err(EvidenceScopeError::PlanMaterialInvalid(format!(
                    "node {} is not selected for gate {} round {}",
                    node.node_id, gate.id, state.current_review_round
                )))
            }
        }
    } else {
        return Err(EvidenceScopeError::PlanMaterialInvalid(format!(
            "node {} is not selected for gate {} round {}",
            node.node_id, gate.id, state.current_review_round
        )));
    };
    Ok(Some(AdmittedGateState {
        gate_id: gate.id.clone(),
        gate_lineage: state.gate_lineage,
        review_round: Some(u32::try_from(review_round).map_err(|_| {
            EvidenceScopeError::PlanMaterialInvalid("review round exceeds u32".into())
        })?),
        required_reviewer_node_ids: gate.required_reviewer_node_ids.clone(),
        resolution_mode: Some(gate.resolution_mode),
    }))
}

async fn build_requirements_identity<C: ConnectionTrait>(
    store: &WorkflowStore<'_, C>,
    manifest: &NormalizedManifest,
    workflow_id: &str,
) -> Result<Option<(RequirementsIdentityV1, String)>, EvidenceScopeError> {
    let Some(design_ref) = manifest.design.as_ref() else {
        return Ok(None);
    };
    let design = verified_document(
        store.workspace_root,
        design_ref,
        "Design",
        MAX_DESIGN_DOCUMENT_BYTES,
    )
    .await?;
    let design_gate_id = manifest
        .gates
        .iter()
        .find(|gate| gate.gate_kind == DocumentGateKind::Design)
        .map(|gate| gate.id.clone());
    let design_settlement_scope = if let Some(gate_id) = design_gate_id {
        delegation_workflow_gate_settlement::Entity::find()
            .filter(
                delegation_workflow_gate_settlement::Column::WorkflowId.eq(workflow_id.to_string()),
            )
            .filter(delegation_workflow_gate_settlement::Column::GateId.eq(gate_id))
            .filter(
                delegation_workflow_gate_settlement::Column::Outcome
                    .eq(GateSettlementOutcome::Approved),
            )
            .order_by_desc(delegation_workflow_gate_settlement::Column::GateCycle)
            .one(store.conn)
            .await
            .map_err(|error| EvidenceScopeError::InstructionBindingFailed(error.to_string()))?
            .and_then(|settlement| settlement.evidence_scope_digest)
    } else {
        None
    };
    if let Some(scope) = design_settlement_scope.as_deref() {
        validate_sha256_token(scope, false)?;
    }
    let identity = RequirementsIdentityV1 {
        design_digest: design.digest,
        design_rel_path: design.rel_path,
        design_settlement_scope,
    };
    let digest = identity.digest()?;
    Ok(Some((identity, digest)))
}

async fn verified_document(
    workspace_root: &Path,
    document: &super::types::DocumentRef,
    label: &str,
    max_bytes: usize,
) -> Result<super::types::DocumentRef, EvidenceScopeError> {
    verified_document_bytes(workspace_root, document, label, max_bytes)
        .await
        .map(|(document, _)| document)
}

async fn load_bound_plan_material<C: ConnectionTrait>(
    store: &WorkflowStore<'_, C>,
    manifest: &NormalizedManifest,
) -> Result<(super::types::DocumentRef, BoundPlanMaterialMap), EvidenceScopeError> {
    let plan_ref = manifest.plan.as_ref().ok_or_else(|| {
        EvidenceScopeError::PlanMaterialInvalid("active manifest has no Plan document".into())
    })?;
    let (plan, bytes) = verified_document_bytes(
        store.workspace_root,
        plan_ref,
        "Plan",
        MAX_PLAN_MATERIAL_BYTES,
    )
    .await?;
    let mut task_indices = manifest
        .task_policies
        .iter()
        .map(|policy| policy.task_index)
        .collect::<Vec<_>>();
    task_indices.sort_unstable();
    let material = parse_plan_material(&bytes, &task_indices)
        .map_err(|error| EvidenceScopeError::PlanMaterialInvalid(error.to_string()))?;
    let bound = bind_plan_material(manifest, &material)
        .map_err(|error| EvidenceScopeError::PlanMaterialInvalid(error.to_string()))?;
    Ok((plan, bound))
}

async fn verified_document_bytes(
    workspace_root: &Path,
    document: &super::types::DocumentRef,
    label: &str,
    max_bytes: usize,
) -> Result<(super::types::DocumentRef, Vec<u8>), EvidenceScopeError> {
    let rel_path = normalize_rel_path(&document.rel_path)
        .map_err(|error| EvidenceScopeError::PlanMaterialInvalid(error.to_string()))?;
    if rel_path != document.rel_path {
        return Err(EvidenceScopeError::PlanMaterialInvalid(format!(
            "{label} path is not canonical"
        )));
    }
    let workspace = workspace_root.to_path_buf();
    let read_path = rel_path.clone();
    let bytes = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
        let canonical_workspace = workspace
            .canonicalize()
            .map_err(|error| format!("workspace is unavailable: {error}"))?;
        if !canonical_workspace.is_dir() {
            return Err("workspace is not a directory".into());
        }
        let canonical_file = canonical_workspace
            .join(PathBuf::from(read_path))
            .canonicalize()
            .map_err(|error| format!("document cannot be resolved: {error}"))?;
        if !canonical_file.starts_with(&canonical_workspace) {
            return Err("document escapes the workspace".into());
        }
        if !canonical_file
            .metadata()
            .map_err(|error| format!("document metadata is unavailable: {error}"))?
            .is_file()
        {
            return Err("document is not a file".into());
        }
        let read_limit = max_bytes
            .checked_add(1)
            .ok_or_else(|| "document size bound overflow".to_string())?;
        let file = std::fs::File::open(canonical_file)
            .map_err(|error| format!("document cannot be opened: {error}"))?;
        let mut bytes = Vec::with_capacity(read_limit.min(64 * 1024));
        file.take(read_limit as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("document cannot be read: {error}"))?;
        Ok(bytes)
    })
    .await
    .map_err(|error| EvidenceScopeError::PlanMaterialInvalid(error.to_string()))?
    .map_err(|error| EvidenceScopeError::PlanMaterialInvalid(error.to_string()))?;
    if bytes.len() > max_bytes {
        return Err(EvidenceScopeError::PlanMaterialInvalid(format!(
            "{label} document exceeds {max_bytes} bytes"
        )));
    }
    let digest = sha256_prefixed(&bytes);
    if digest != document.digest {
        return Err(EvidenceScopeError::PlanMaterialInvalid(format!(
            "{label} document digest does not match durable manifest"
        )));
    }
    Ok((super::types::DocumentRef { rel_path, digest }, bytes))
}

fn sha256_prefixed(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn require_identity(identity: Option<&str>, label: &str) -> Result<String, EvidenceScopeError> {
    let identity = identity
        .ok_or_else(|| EvidenceScopeError::PlanMaterialInvalid(format!("missing {label}")))?;
    validate_sha256_token(identity, false)?;
    Ok(identity.to_string())
}

fn task_specification_digest(
    identity: &TaskSpecificationIdentityV1,
) -> Result<String, EvidenceScopeError> {
    canonical_json_sha256(
        "codeg.completion.task_specification.v1",
        DIGEST_SCHEMA_VERSION,
        identity,
    )
}

fn policy_digest<T: Serialize>(value: &T) -> Result<String, EvidenceScopeError> {
    canonical_json_sha256("codeg.completion.policy.v2", DIGEST_SCHEMA_VERSION, value)
}

fn plan_reviewer_policy_digest(
    manifest: &NormalizedManifest,
    reviewer: &delegation_workflow_node_binding::Model,
    gate_id: Option<&str>,
) -> Result<String, EvidenceScopeError> {
    let selected_tasks = manifest
        .task_policies
        .iter()
        .filter_map(|policy| {
            let covers_task = policy.route.reviewer_node_ids.iter().any(|node_id| {
                manifest.nodes.iter().any(|node| {
                    node.id.as_str() == node_id.as_str()
                        && node.agent_type.as_deref() == Some(reviewer.agent_type.as_str())
                        && node.profile_id == reviewer.profile_id
                })
            });
            covers_task.then_some(json!({
                "task_index": policy.task_index,
                "risk": policy.risk,
            }))
        })
        .collect::<Vec<_>>();
    policy_digest(&json!({
        "gate_id": gate_id,
        "reviewer_node_id": reviewer.node_id,
        "risk_policy_version": manifest.risk_policy_version,
        "selected_tasks": selected_tasks,
    }))
}

fn task_policy(
    manifest: &NormalizedManifest,
    task_index: u32,
) -> Result<&super::types::ManifestTaskPolicy, EvidenceScopeError> {
    manifest
        .task_policies
        .iter()
        .find(|policy| policy.task_index == task_index)
        .ok_or_else(|| {
            EvidenceScopeError::PlanMaterialInvalid(format!(
                "active manifest has no policy for Task {task_index}"
            ))
        })
}

fn task_route_digest(
    manifest: &NormalizedManifest,
    task_index: u32,
) -> Result<String, EvidenceScopeError> {
    let route = &task_policy(manifest, task_index)?.route;
    canonical_json_sha256(
        "codeg.completion.route.v2",
        DIGEST_SCHEMA_VERSION,
        &json!({
            "implementer_node_id": route.implementer_node_id,
        }),
    )
}

fn task_dependency_identities(
    manifest: &NormalizedManifest,
    node_id: &str,
) -> Result<Vec<String>, EvidenceScopeError> {
    let mut dependencies = manifest
        .edges
        .iter()
        .filter(|edge| edge.to == node_id)
        .map(|edge| ScopeEdgeV2 {
            from: edge.from.clone(),
            to: edge.to.clone(),
        })
        .collect::<Vec<_>>();
    dependencies.sort_by(|left, right| (&left.from, &left.to).cmp(&(&right.from, &right.to)));
    dependencies
        .iter()
        .map(|edge| {
            canonical_json_sha256(
                "codeg.completion.dependencies.v2",
                DIGEST_SCHEMA_VERSION,
                edge,
            )
        })
        .collect()
}

fn final_review_requirements_digest(
    manifest: &NormalizedManifest,
    reviewer_node_id: &str,
) -> Result<String, EvidenceScopeError> {
    let is_final_reviewer = manifest.nodes.iter().any(|node| {
        node.id == reviewer_node_id
            && node.phase_id.as_deref() == Some(PHASE_FINAL)
            && node.role == Some(super::types::ManifestNodeRole::Reviewer)
    });
    if !is_final_reviewer {
        return Err(EvidenceScopeError::PlanMaterialInvalid(format!(
            "Final Reviewer node {reviewer_node_id} is absent from the active manifest"
        )));
    }
    let mut incoming_edges = manifest
        .edges
        .iter()
        .filter(|edge| edge.to == reviewer_node_id)
        .map(|edge| ScopeEdgeV2 {
            from: edge.from.clone(),
            to: edge.to.clone(),
        })
        .collect::<Vec<_>>();
    incoming_edges.sort_by(|left, right| (&left.from, &left.to).cmp(&(&right.from, &right.to)));
    policy_digest(&json!({
        "reviewer_node_id": reviewer_node_id,
        "incoming_edges": incoming_edges,
    }))
}

async fn active_plan_identity<C: ConnectionTrait>(
    store: &WorkflowStore<'_, C>,
    manifest: &NormalizedManifest,
    workflow: &delegation_workflow::Model,
) -> Result<String, EvidenceScopeError> {
    let plan_ref = manifest.plan.as_ref().ok_or_else(|| {
        EvidenceScopeError::PlanMaterialInvalid("active manifest has no Plan document".into())
    })?;
    let plan = verified_document(
        store.workspace_root,
        plan_ref,
        "Plan",
        MAX_PLAN_MATERIAL_BYTES,
    )
    .await?;
    let plan_gate = manifest
        .gates
        .iter()
        .find(|gate| gate.gate_kind == DocumentGateKind::Plan)
        .ok_or_else(|| {
            EvidenceScopeError::PlanMaterialInvalid("active manifest has no Plan gate".into())
        })?;
    let settlement = delegation_workflow_gate_settlement::Entity::find()
        .filter(
            delegation_workflow_gate_settlement::Column::WorkflowId
                .eq(workflow.workflow_id.clone()),
        )
        .filter(delegation_workflow_gate_settlement::Column::GateId.eq(plan_gate.id.clone()))
        .order_by_desc(delegation_workflow_gate_settlement::Column::GateCycle)
        .one(store.conn)
        .await
        .map_err(|error| EvidenceScopeError::InstructionBindingFailed(error.to_string()))?
        .ok_or_else(|| {
            EvidenceScopeError::PlanMaterialInvalid(
                "Task/Final admission requires an Approved Plan settlement".into(),
            )
        })?;
    if settlement.outcome != GateSettlementOutcome::Approved
        || settlement.covered_plan_digest.as_deref() != Some(plan.digest.as_str())
    {
        return Err(EvidenceScopeError::PlanMaterialInvalid(
            "latest Plan settlement is not Approved for the active Plan".into(),
        ));
    }
    let gate_lineage = require_identity(settlement.gate_lineage.as_deref(), "Plan gate lineage")?;
    let evidence_scope_digest = require_identity(
        settlement.evidence_scope_digest.as_deref(),
        "Plan settlement evidence scope",
    )?;
    let review_round = settlement
        .review_round
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            EvidenceScopeError::PlanMaterialInvalid(
                "Approved Plan settlement has no valid review round".into(),
            )
        })?;
    let gate_state = delegation_workflow_gate_state::Entity::find_by_id((
        workflow.workflow_id.clone(),
        plan_gate.id.clone(),
    ))
    .one(store.conn)
    .await
    .map_err(|error| EvidenceScopeError::InstructionBindingFailed(error.to_string()))?
    .ok_or_else(|| {
        EvidenceScopeError::PlanMaterialInvalid("Plan gate has no durable lineage state".into())
    })?;
    if gate_state.gate_lineage != gate_lineage {
        return Err(EvidenceScopeError::PlanMaterialInvalid(
            "Approved Plan settlement is stale for the current gate lineage".into(),
        ));
    }
    canonical_json_sha256(
        "codeg.completion.plan_identity.v2",
        DIGEST_SCHEMA_VERSION,
        &json!({
            "plan": plan,
            "gate_lineage": gate_lineage,
            "review_round": review_round,
            "evidence_scope_digest": evidence_scope_digest,
        }),
    )
}

fn require_git_subject(
    candidate: &AdmissionCandidate<'_>,
    role: CompletionScopeRole,
) -> Result<ArtifactSubjectIdentityV2, EvidenceScopeError> {
    let digest = match role {
        CompletionScopeRole::TaskImplementer | CompletionScopeRole::FinalFixer => {
            candidate.producer_baseline_head
        }
        CompletionScopeRole::TaskReviewer | CompletionScopeRole::FinalReviewer => {
            candidate.artifact_digest
        }
        _ => None,
    }
    .ok_or_else(|| {
        EvidenceScopeError::InstructionBindingFailed(format!(
            "{role:?} requires a durable git artifact subject"
        ))
    })?;
    validate_digest_or_git_token("head", digest)?;
    Ok(ArtifactSubjectIdentityV2::GitHeadV1 {
        digest: digest.to_string(),
    })
}

fn git_identity_digest(head: &str) -> Result<String, EvidenceScopeError> {
    canonical_json_sha256(
        "codeg.completion.dependencies.v2",
        DIGEST_SCHEMA_VERSION,
        &json!({ "head": head }),
    )
}

async fn active_final_findings_package<C: ConnectionTrait>(
    store: &WorkflowStore<'_, C>,
    workflow: &delegation_workflow::Model,
    gate: &AdmittedGateState,
) -> Result<FinalFindingsPackageV1, EvidenceScopeError> {
    let package = delegation_final_findings_package::Entity::find()
        .filter(
            delegation_final_findings_package::Column::WorkflowId.eq(workflow.workflow_id.clone()),
        )
        .filter(
            delegation_final_findings_package::Column::Status
                .eq(FinalFindingsPackageStatus::Active),
        )
        .filter(delegation_final_findings_package::Column::GateId.eq(gate.gate_id.clone()))
        .filter(
            delegation_final_findings_package::Column::GateLineage.eq(gate.gate_lineage.clone()),
        )
        .one(store.conn)
        .await
        .map_err(|error| EvidenceScopeError::InstructionBindingFailed(error.to_string()))?
        .ok_or_else(|| {
            EvidenceScopeError::InstructionBindingFailed(
                "Final Fixer requires an active durable findings package".into(),
            )
        })?;
    let package = verify_final_findings_package_model_v1(&package)
        .map_err(|error| EvidenceScopeError::EvidenceCorrupt(error.to_string()))?;
    validate_sha256_token(package.final_findings_identity(), false)?;
    Ok(package)
}

async fn ordered_task_output_identities<C: ConnectionTrait>(
    store: &WorkflowStore<'_, C>,
    manifest: &NormalizedManifest,
    workflow: &delegation_workflow::Model,
) -> Result<Vec<String>, EvidenceScopeError> {
    let mut policies = manifest.task_policies.iter().collect::<Vec<_>>();
    policies.sort_by_key(|policy| policy.task_index);
    let mut identities = Vec::with_capacity(policies.len());
    for policy in policies {
        let binding = delegation_workflow_run_binding::Entity::find()
            .filter(
                delegation_workflow_run_binding::Column::WorkflowId
                    .eq(workflow.workflow_id.clone()),
            )
            .filter(
                delegation_workflow_run_binding::Column::NodeId
                    .eq(policy.route.implementer_node_id.clone()),
            )
            .order_by_desc(delegation_workflow_run_binding::Column::LineageOrdinal)
            .one(store.conn)
            .await
            .map_err(|error| EvidenceScopeError::InstructionBindingFailed(error.to_string()))?
            .ok_or_else(|| {
                EvidenceScopeError::InstructionBindingFailed(format!(
                    "Final Reviewer has no Task {} output binding",
                    policy.task_index
                ))
            })?;
        let artifact_digest = binding.artifact_digest.ok_or_else(|| {
            EvidenceScopeError::InstructionBindingFailed(format!(
                "Task {} output has no durable artifact",
                policy.task_index
            ))
        })?;
        validate_digest_or_git_token("head", &artifact_digest)?;
        let run = delegation_task_run::Entity::find_by_id(binding.task_id.clone())
            .one(store.conn)
            .await
            .map_err(|error| EvidenceScopeError::InstructionBindingFailed(error.to_string()))?
            .ok_or_else(|| {
                EvidenceScopeError::InstructionBindingFailed(format!(
                    "Task {} output run is missing",
                    policy.task_index
                ))
            })?;
        identities.push(task_output_identity_digest(
            policy.task_index,
            &policy.route.implementer_node_id,
            &binding.task_id,
            run.generation,
            &artifact_digest,
        )?);
    }
    Ok(identities)
}

fn task_output_identity_digest(
    task_index: u32,
    node_id: &str,
    producer_task_id: &str,
    producer_generation: i64,
    artifact_head: &str,
) -> Result<String, EvidenceScopeError> {
    if node_id.trim().is_empty() || producer_task_id.trim().is_empty() || producer_generation < 0 {
        return Err(EvidenceScopeError::InstructionBindingFailed(
            "invalid selected Task producer identity".into(),
        ));
    }
    validate_digest_or_git_token("artifact_head", artifact_head)?;
    canonical_json_sha256(
        "codeg.completion.dependencies.v2",
        DIGEST_SCHEMA_VERSION,
        &json!({
            "role": "task_implementer",
            "task_index": task_index,
            "node_id": node_id,
            "producer_task_id": producer_task_id,
            "producer_generation": producer_generation,
            "artifact_head": artifact_head,
        }),
    )
}

pub fn canonical_json_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, EvidenceScopeError> {
    reject_float_values(value)?;
    let serialized = serde_json::to_vec(value)
        .map_err(|error| EvidenceScopeError::InvalidCanonicalJson(error.to_string()))?;
    if serialized.len() > MAX_CANONICAL_JSON_BYTES {
        return Err(EvidenceScopeError::InvalidCanonicalJson(format!(
            "canonical value exceeds {MAX_CANONICAL_JSON_BYTES} bytes"
        )));
    }
    let mut deserializer = serde_json::Deserializer::from_slice(&serialized);
    let value = UniqueJsonValue
        .deserialize(&mut deserializer)
        .map_err(|error| EvidenceScopeError::InvalidCanonicalJson(error.to_string()))?;
    deserializer
        .end()
        .map_err(|error| EvidenceScopeError::InvalidCanonicalJson(error.to_string()))?;
    let canonical = canonicalize_value(value, None)?;
    let bytes = serde_json::to_vec(&canonical)
        .map_err(|error| EvidenceScopeError::InvalidCanonicalJson(error.to_string()))?;
    if bytes.len() > MAX_CANONICAL_JSON_BYTES {
        return Err(EvidenceScopeError::InvalidCanonicalJson(format!(
            "canonical value exceeds {MAX_CANONICAL_JSON_BYTES} bytes"
        )));
    }
    Ok(bytes)
}

fn reject_float_values<T: Serialize + ?Sized>(value: &T) -> Result<(), EvidenceScopeError> {
    // serde_json coerces non-finite floats to null, so validate the source
    // Serialize graph before any JSON representation can erase the type.
    value.serialize(FloatRejectingSerializer)
}

impl serde::ser::Error for EvidenceScopeError {
    fn custom<T: std::fmt::Display>(message: T) -> Self {
        Self::InvalidCanonicalJson(message.to_string())
    }
}

#[derive(Clone, Copy)]
struct FloatRejectingSerializer;

impl serde::Serializer for FloatRejectingSerializer {
    type Ok = ();
    type Error = EvidenceScopeError;
    type SerializeSeq = FloatRejectingCompound;
    type SerializeTuple = FloatRejectingCompound;
    type SerializeTupleStruct = FloatRejectingCompound;
    type SerializeTupleVariant = FloatRejectingCompound;
    type SerializeMap = FloatRejectingCompound;
    type SerializeStruct = FloatRejectingCompound;
    type SerializeStructVariant = FloatRejectingCompound;

    fn serialize_bool(self, _value: bool) -> Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_i8(self, _value: i8) -> Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_i16(self, _value: i16) -> Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_i32(self, _value: i32) -> Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_i64(self, _value: i64) -> Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_i128(self, _value: i128) -> Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_u8(self, _value: u8) -> Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_u16(self, _value: u16) -> Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_u32(self, _value: u32) -> Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_u64(self, _value: u64) -> Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_u128(self, _value: u128) -> Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_f32(self, _value: f32) -> Result<Self::Ok, Self::Error> {
        Err(EvidenceScopeError::InvalidCanonicalJson(
            "floating-point values are forbidden".into(),
        ))
    }

    fn serialize_f64(self, _value: f64) -> Result<Self::Ok, Self::Error> {
        Err(EvidenceScopeError::InvalidCanonicalJson(
            "floating-point values are forbidden".into(),
        ))
    }

    fn serialize_char(self, _value: char) -> Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_str(self, _value: &str) -> Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_bytes(self, _value: &[u8]) -> Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_none(self) -> Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<Self::Ok, Self::Error> {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
    ) -> Result<Self::Ok, Self::Error> {
        Ok(())
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error> {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error> {
        value.serialize(self)
    }

    fn serialize_seq(self, _length: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
        Ok(FloatRejectingCompound)
    }

    fn serialize_tuple(self, _length: usize) -> Result<Self::SerializeTuple, Self::Error> {
        Ok(FloatRejectingCompound)
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _length: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        Ok(FloatRejectingCompound)
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _length: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        Ok(FloatRejectingCompound)
    }

    fn serialize_map(self, _length: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        Ok(FloatRejectingCompound)
    }

    fn serialize_struct(
        self,
        _name: &'static str,
        _length: usize,
    ) -> Result<Self::SerializeStruct, Self::Error> {
        Ok(FloatRejectingCompound)
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _length: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        Ok(FloatRejectingCompound)
    }
}

struct FloatRejectingCompound;

impl SerializeSeq for FloatRejectingCompound {
    type Ok = ();
    type Error = EvidenceScopeError;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
        value.serialize(FloatRejectingSerializer)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(())
    }
}

impl SerializeTuple for FloatRejectingCompound {
    type Ok = ();
    type Error = EvidenceScopeError;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
        value.serialize(FloatRejectingSerializer)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(())
    }
}

impl SerializeTupleStruct for FloatRejectingCompound {
    type Ok = ();
    type Error = EvidenceScopeError;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
        value.serialize(FloatRejectingSerializer)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(())
    }
}

impl SerializeTupleVariant for FloatRejectingCompound {
    type Ok = ();
    type Error = EvidenceScopeError;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
        value.serialize(FloatRejectingSerializer)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(())
    }
}

impl SerializeMap for FloatRejectingCompound {
    type Ok = ();
    type Error = EvidenceScopeError;

    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), Self::Error> {
        key.serialize(FloatRejectingSerializer)
    }

    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
        value.serialize(FloatRejectingSerializer)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(())
    }
}

impl SerializeStruct for FloatRejectingCompound {
    type Ok = ();
    type Error = EvidenceScopeError;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        _key: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        value.serialize(FloatRejectingSerializer)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(())
    }
}

impl SerializeStructVariant for FloatRejectingCompound {
    type Ok = ();
    type Error = EvidenceScopeError;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        _key: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        value.serialize(FloatRejectingSerializer)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(())
    }
}

struct UniqueJsonValue;

impl<'de> DeserializeSeed<'de> for UniqueJsonValue {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(UniqueJsonValueVisitor)
    }
}

struct UniqueJsonValueVisitor;

impl<'de> Visitor<'de> for UniqueJsonValueVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(Value::String(value.to_string()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(Value::String(value))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        UniqueJsonValue.deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::with_capacity(sequence.size_hint().unwrap_or(0));
        while let Some(value) = sequence.next_element_seed(UniqueJsonValue)? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = Map::new();
        while let Some(key) = object.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(serde::de::Error::custom(format!(
                    "duplicate semantic key {key}"
                )));
            }
            values.insert(key, object.next_value_seed(UniqueJsonValue)?);
        }
        Ok(Value::Object(values))
    }
}

pub fn canonical_json_sha256<T: Serialize>(
    domain: &str,
    schema_version: u32,
    value: &T,
) -> Result<String, EvidenceScopeError> {
    validate_domain(domain, schema_version)?;
    let bytes = canonical_json_bytes(value)?;
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update([0]);
    hasher.update(schema_version.to_string().as_bytes());
    hasher.update([0]);
    hasher.update(bytes);
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

pub fn validate_digest_domain(
    domain: &str,
    schema_version: u32,
    digest: &str,
) -> Result<(), EvidenceScopeError> {
    validate_domain(domain, schema_version)?;
    validate_sha256_token(digest, true)
}

pub fn material_identity_summaries(
    material: &PlanMaterialMap,
    selector: &MaterialSelectorV1,
) -> Result<Vec<MaterialIdentitySummary>, EvidenceScopeError> {
    let material_value = serde_json::to_value(material)
        .map_err(|error| EvidenceScopeError::PlanMaterialInvalid(error.to_string()))?;
    let entries = material_value
        .get("materials")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            EvidenceScopeError::PlanMaterialInvalid(
                "serialized Plan material has no material map".into(),
            )
        })?;
    let selector_value = serde_json::to_value(selector)
        .map_err(|error| EvidenceScopeError::PlanMaterialInvalid(error.to_string()))?;
    let kind = selector_value
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| EvidenceScopeError::PlanMaterialInvalid("selector has no kind".into()))?;
    let selected: BTreeSet<String> = match kind {
        "all" => entries.keys().cloned().collect(),
        "keys" => {
            let keys = selector_value
                .get("keys")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    EvidenceScopeError::PlanMaterialInvalid("key selector has no key array".into())
                })?;
            let mut selected = BTreeSet::new();
            for key in keys {
                let key = key.as_str().ok_or_else(|| {
                    EvidenceScopeError::PlanMaterialInvalid("selector key is not a string".into())
                })?;
                if key == "plan.global_*" {
                    selected.insert("plan.global_constraints".into());
                    selected.insert("plan.global_preamble".into());
                } else {
                    selected.insert(key.to_string());
                }
            }
            selected
        }
        other => {
            return Err(EvidenceScopeError::PlanMaterialInvalid(format!(
                "unsupported selector kind {other}"
            )))
        }
    };

    selected
        .into_iter()
        .map(|key| {
            let entry = entries
                .get(&key)
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    EvidenceScopeError::PlanMaterialInvalid(format!(
                        "selector references missing material key {key}"
                    ))
                })?;
            let body_sha256 = entry
                .get("body_sha256")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    EvidenceScopeError::PlanMaterialInvalid(format!(
                        "material key {key} has no body digest"
                    ))
                })?;
            validate_sha256_token(body_sha256, false)?;
            Ok(MaterialIdentitySummary {
                key,
                body_sha256: body_sha256.to_string(),
            })
        })
        .collect()
}

pub fn build_instruction_block(
    input: &InstructionBlockInput<'_>,
) -> Result<InstructionBlockV1, EvidenceScopeError> {
    validate_instruction_input(input)?;
    #[derive(Serialize)]
    struct CanonicalInstruction<'a> {
        template_id: &'static str,
        template_version: u32,
        role: CompletionRole,
        phase_id: &'a str,
        task_index: Option<u32>,
        gate_id: Option<&'a str>,
        review_round: Option<u32>,
        conclusion_suffix: &'static str,
        material_identities: &'a [MaterialIdentitySummary],
    }
    let canonical = CanonicalInstruction {
        template_id: INSTRUCTION_TEMPLATE_ID,
        template_version: INSTRUCTION_TEMPLATE_VERSION,
        role: input.role,
        phase_id: input.phase_id,
        task_index: input.task_index,
        gate_id: input.gate_id,
        review_round: input.review_round,
        conclusion_suffix: build_conclusion_suffix(input.role),
        material_identities: input.material_identities,
    };
    let bytes = canonical_json_bytes(&canonical)?;
    if bytes.len() > MAX_INSTRUCTION_BLOCK_BYTES {
        return Err(EvidenceScopeError::InstructionBindingFailed(format!(
            "instruction exceeds {MAX_INSTRUCTION_BLOCK_BYTES} bytes"
        )));
    }
    let digest = canonical_json_sha256(INSTRUCTION_DOMAIN, DIGEST_SCHEMA_VERSION, &canonical)?;
    let canonical_utf8 = String::from_utf8(bytes)
        .map_err(|error| EvidenceScopeError::InstructionBindingFailed(error.to_string()))?;
    Ok(InstructionBlockV1 {
        template_id: INSTRUCTION_TEMPLATE_ID.into(),
        template_version: INSTRUCTION_TEMPLATE_VERSION,
        canonical_utf8,
        digest,
    })
}

fn build_final_fixer_instruction_block(
    input: &InstructionBlockInput<'_>,
    package: &FinalFindingsPackageV1,
) -> Result<InstructionBlockV1, EvidenceScopeError> {
    let base = build_instruction_block(input)?;
    let base_value: Value = serde_json::from_str(&base.canonical_utf8)
        .map_err(|error| EvidenceScopeError::EvidenceCorrupt(error.to_string()))?;
    let canonical = json!({
        "instruction": base_value,
        "final_findings_identity": package.final_findings_identity(),
        "remediation_contexts": package.remediation_contexts,
    });
    let bytes = canonical_json_bytes(&canonical)?;
    if bytes.len() > MAX_FINAL_FIXER_INSTRUCTION_BYTES {
        return Err(EvidenceScopeError::InstructionBindingFailed(format!(
            "Final Fixer instruction exceeds {MAX_FINAL_FIXER_INSTRUCTION_BYTES} bytes"
        )));
    }
    let digest = canonical_json_sha256(INSTRUCTION_DOMAIN, DIGEST_SCHEMA_VERSION, &canonical)?;
    let canonical_utf8 = String::from_utf8(bytes)
        .map_err(|error| EvidenceScopeError::InstructionBindingFailed(error.to_string()))?;
    Ok(InstructionBlockV1 {
        template_id: INSTRUCTION_TEMPLATE_ID.into(),
        template_version: INSTRUCTION_TEMPLATE_VERSION,
        canonical_utf8,
        digest,
    })
}

pub fn review_scope_digest(
    scope: &RoleReviewScopeV2,
    instruction: &InstructionBlockV1,
) -> Result<String, EvidenceScopeError> {
    validate_sha256_token(&instruction.digest, false)?;
    canonical_json_sha256(
        REVIEW_SCOPE_DOMAIN,
        DIGEST_SCHEMA_VERSION,
        &json!({
            "instruction_block_digest": instruction.digest,
            "role_scope": scope,
        }),
    )
}

pub fn evidence_scope_digest(scope: &EvidenceScopeInputV2) -> Result<String, EvidenceScopeError> {
    validate_evidence_scope_input(scope)?;
    canonical_json_sha256(EVIDENCE_SCOPE_DOMAIN, DIGEST_SCHEMA_VERSION, scope)
}

pub fn validate_completion_evidence(
    evidence_json: &str,
    current: &EvidenceValidationContext,
) -> Result<ValidatedCompletionEvidence, EvidenceScopeError> {
    if evidence_json.len() > MAX_EVIDENCE_JSON_BYTES {
        return Err(EvidenceScopeError::EvidenceCorrupt(format!(
            "evidence exceeds {MAX_EVIDENCE_JSON_BYTES} bytes"
        )));
    }
    reject_unknown_intent_fields(evidence_json)?;
    let evidence: CompletionEvidenceV2 = serde_json::from_str(evidence_json)
        .map_err(|error| EvidenceScopeError::EvidenceCorrupt(error.to_string()))?;
    if evidence.version != COMPLETION_PROTOCOL_VERSION_V2 {
        return Err(EvidenceScopeError::EvidenceCorrupt(
            "unsupported evidence version".into(),
        ));
    }
    if current.role != evidence.binding.role || !current.role.accepts(evidence.intent.outcome) {
        return Err(EvidenceScopeError::OutcomeRoleMismatch);
    }
    validate_intent(&evidence.intent)?;
    validate_completion_artifact(&evidence.artifact)?;
    validate_evidence_scope_input(&current.scope)?;
    if !binding_semantically_matches(&evidence.binding, &current.binding)
        || evidence.artifact != current.artifact
        || evidence.review_scope_digest != current.scope.review_scope_digest
        || !scope_matches_binding(&current.scope, &current.binding)
    {
        return Err(EvidenceScopeError::ScopeChanged);
    }
    let current_digest = evidence_scope_digest(&current.scope)?;
    if evidence.evidence_scope_digest != current_digest {
        return Err(EvidenceScopeError::ScopeChanged);
    }
    if evidence.captured_at.trim().is_empty() {
        return Err(EvidenceScopeError::EvidenceCorrupt(
            "captured_at is empty".into(),
        ));
    }
    Ok(ValidatedCompletionEvidence {
        evidence,
        evidence_validated: true,
    })
}

pub fn admitted_evidence_scope_digest(
    context: &AdmissionCompletionContextV2,
) -> Result<String, EvidenceScopeError> {
    evidence_scope_digest(&context.evidence_scope)
}

impl MaterialSelectorV1 {
    pub fn digest(&self) -> String {
        canonical_json_sha256(
            "codeg.completion.material_selector.v1",
            DIGEST_SCHEMA_VERSION,
            self,
        )
        .expect("validated material selectors must canonicalize")
    }
}

impl RequirementsIdentityV1 {
    pub fn digest(&self) -> Result<String, EvidenceScopeError> {
        canonical_json_sha256(
            "codeg.completion.requirements.v1",
            DIGEST_SCHEMA_VERSION,
            self,
        )
    }
}

impl PlanSubjectIdentityV2 {
    pub fn digest(&self) -> Result<String, EvidenceScopeError> {
        canonical_json_sha256(
            "codeg.completion.plan_subject.v2",
            DIGEST_SCHEMA_VERSION,
            self,
        )
    }
}

fn validate_domain(domain: &str, schema_version: u32) -> Result<(), EvidenceScopeError> {
    if schema_version != DIGEST_SCHEMA_VERSION || !ALLOWED_DIGEST_DOMAINS.contains(&domain) {
        return Err(EvidenceScopeError::UnsupportedDomain);
    }
    Ok(())
}

fn canonicalize_value(value: Value, key: Option<&str>) -> Result<Value, EvidenceScopeError> {
    match value {
        Value::Null | Value::Bool(_) => Ok(value),
        Value::Number(number) => {
            if number.is_f64() {
                Err(EvidenceScopeError::InvalidCanonicalJson(
                    "floating-point values are forbidden".into(),
                ))
            } else {
                Ok(Value::Number(number))
            }
        }
        Value::String(value) => canonicalize_string(key, &value).map(Value::String),
        Value::Array(values) => values
            .into_iter()
            .map(|value| canonicalize_value(value, key))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(values) => {
            let mut sorted = BTreeMap::new();
            for (key, value) in values {
                if sorted.contains_key(&key) {
                    return Err(EvidenceScopeError::InvalidCanonicalJson(format!(
                        "duplicate semantic key {key}"
                    )));
                }
                let normalized_key: String = key.nfc().collect();
                if normalized_key != key || normalized_key.is_empty() {
                    return Err(EvidenceScopeError::InvalidCanonicalJson(
                        "object keys must be non-empty NFC strings".into(),
                    ));
                }
                sorted.insert(key.clone(), canonicalize_value(value, Some(&key))?);
            }
            let mut map = Map::new();
            for (key, value) in sorted {
                map.insert(key, value);
            }
            Ok(Value::Object(map))
        }
    }
}

fn canonicalize_string(key: Option<&str>, value: &str) -> Result<String, EvidenceScopeError> {
    if value.contains('\0') {
        return Err(EvidenceScopeError::InvalidCanonicalJson(
            "NUL characters are forbidden".into(),
        ));
    }
    let normalized: String = value.nfc().collect();
    if key.is_some_and(is_path_key) {
        return normalize_rel_path(&normalized)
            .map_err(|error| EvidenceScopeError::InvalidCanonicalJson(error.to_string()));
    }
    if key.is_some_and(is_digest_key) {
        validate_digest_or_git_token(key.unwrap_or_default(), &normalized)?;
    }
    if key.is_some_and(is_lowercase_token_key)
        && normalized
            .chars()
            .any(|ch| ch.is_ascii_uppercase() || ch.is_whitespace())
    {
        return Err(EvidenceScopeError::InvalidCanonicalJson(format!(
            "{} must be a lowercase token",
            key.unwrap_or_default()
        )));
    }
    Ok(normalized)
}

fn is_path_key(key: &str) -> bool {
    key == "rel_path"
        || key == "report_file"
        || key == "plan_target_rel_path"
        || key.ends_with("_rel_path")
}

fn is_digest_key(key: &str) -> bool {
    key == "digest"
        || key == "head"
        || key == "gate_lineage"
        || key.ends_with("_digest")
        || key.ends_with("_sha256")
        || key.ends_with("_head")
        || key.ends_with("_identity")
}

fn is_lowercase_token_key(key: &str) -> bool {
    matches!(
        key,
        "role"
            | "kind"
            | "phase_id"
            | "outcome"
            | "source"
            | "availability"
            | "workflow_kind"
            | "template_id"
            | "classifier_version"
            | "risk_policy_version"
    )
}

fn validate_digest_or_git_token(key: &str, value: &str) -> Result<(), EvidenceScopeError> {
    if (key == "head" || key == "digest" || key.ends_with("_head") || key == "branch_tip")
        && (is_lower_hex(value, 40) || is_lower_hex(value, 64))
    {
        return Ok(());
    }
    validate_sha256_token(value, value == "sha256:00")
}

fn validate_sha256_token(value: &str, allow_short_fixture: bool) -> Result<(), EvidenceScopeError> {
    if allow_short_fixture && value == "sha256:00" {
        return Ok(());
    }
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(EvidenceScopeError::InvalidCanonicalJson(
            "digest must use lowercase sha256 prefix".into(),
        ));
    };
    if !is_lower_hex(hex, 64) {
        return Err(EvidenceScopeError::InvalidCanonicalJson(
            "digest must contain 64 lowercase hexadecimal characters".into(),
        ));
    }
    Ok(())
}

fn is_lower_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_instruction_input(input: &InstructionBlockInput<'_>) -> Result<(), EvidenceScopeError> {
    if input.phase_id.trim().is_empty()
        || input.phase_id.chars().any(|ch| ch.is_ascii_uppercase())
        || input.review_round == Some(0)
    {
        return Err(EvidenceScopeError::InstructionBindingFailed(
            "invalid phase or review round".into(),
        ));
    }
    if input.role == CompletionRole::Reviewer
        && input.phase_id == "tasks"
        && input.task_index.is_none()
    {
        return Err(EvidenceScopeError::InstructionBindingFailed(
            "Task Reviewer instruction requires task_index".into(),
        ));
    }
    if input.role != CompletionRole::Reviewer && input.review_round.is_some() {
        return Err(EvidenceScopeError::InstructionBindingFailed(
            "producer instruction cannot carry review_round".into(),
        ));
    }
    let mut prior = None;
    for identity in input.material_identities {
        validate_sha256_token(&identity.body_sha256, false)?;
        if identity.key.trim().is_empty()
            || prior.is_some_and(|value| value >= identity.key.as_str())
        {
            return Err(EvidenceScopeError::InstructionBindingFailed(
                "material identities must be uniquely sorted".into(),
            ));
        }
        prior = Some(identity.key.as_str());
    }
    Ok(())
}

fn validate_evidence_scope_input(scope: &EvidenceScopeInputV2) -> Result<(), EvidenceScopeError> {
    if scope.completion_protocol_version != COMPLETION_PROTOCOL_VERSION_V2
        || scope.scope_schema_version != EVIDENCE_SCOPE_SCHEMA_VERSION_V2
        || scope.workflow_id.trim().is_empty()
        || scope.node.node_id.trim().is_empty()
        || scope.node.phase_id.trim().is_empty()
        || scope.node.work_unit_key.trim().is_empty()
        || scope.review_round == Some(0)
        || (scope.node.role != CompletionRole::Reviewer && scope.review_round.is_some())
    {
        return Err(EvidenceScopeError::EvidenceCorrupt(
            "invalid common evidence scope".into(),
        ));
    }
    validate_sha256_token(&scope.instruction_block_digest, false)?;
    validate_sha256_token(&scope.review_scope_digest, false)?;
    if let Some(lineage) = scope.gate_lineage.as_deref() {
        validate_sha256_token(lineage, false)?;
    }
    match &scope.artifact_subject {
        ArtifactSubjectIdentityV2::DocumentSha256 { rel_path, digest } => {
            require_canonical_path(rel_path)?;
            validate_sha256_token(digest, false)?;
        }
        ArtifactSubjectIdentityV2::GitHeadV1 { digest } => {
            validate_digest_or_git_token("head", digest)?;
        }
        ArtifactSubjectIdentityV2::PlanMaterial {
            plan_rel_path,
            gate_lineage,
            material_selector_digest,
            selected_material_digest,
        } => {
            require_canonical_path(plan_rel_path)?;
            validate_sha256_token(gate_lineage, false)?;
            validate_sha256_token(material_selector_digest, false)?;
            validate_sha256_token(selected_material_digest, false)?;
        }
        ArtifactSubjectIdentityV2::PendingDocument { rel_path } => {
            require_canonical_path(rel_path)?;
        }
    }
    if let Some(producer) = &scope.reviewed_producer {
        if producer.task_id.trim().is_empty() || producer.generation < 0 {
            return Err(EvidenceScopeError::EvidenceCorrupt(
                "invalid reviewed producer".into(),
            ));
        }
    }
    Ok(())
}

fn require_canonical_path(value: &str) -> Result<(), EvidenceScopeError> {
    let normalized = normalize_rel_path(value)
        .map_err(|error| EvidenceScopeError::EvidenceCorrupt(error.to_string()))?;
    if normalized != value {
        return Err(EvidenceScopeError::EvidenceCorrupt(
            "path is not canonical".into(),
        ));
    }
    Ok(())
}

fn reject_unknown_intent_fields(evidence_json: &str) -> Result<(), EvidenceScopeError> {
    let value: Value = serde_json::from_str(evidence_json)
        .map_err(|error| EvidenceScopeError::EvidenceCorrupt(error.to_string()))?;
    let intent = value
        .get("intent")
        .and_then(Value::as_object)
        .ok_or_else(|| EvidenceScopeError::EvidenceCorrupt("intent is missing".into()))?;
    if intent.keys().any(|key| {
        !matches!(
            key.as_str(),
            "outcome" | "summary" | "report_file" | "source"
        )
    }) {
        return Err(EvidenceScopeError::EvidenceCorrupt(
            "intent contains unknown fields".into(),
        ));
    }
    Ok(())
}

fn validate_intent(intent: &super::CompletionIntent) -> Result<(), EvidenceScopeError> {
    if !intent.source.is_platform_supported() {
        return Err(EvidenceScopeError::EvidenceCorrupt(
            "unsupported completion source".into(),
        ));
    }
    if intent
        .summary
        .as_ref()
        .is_some_and(|summary| summary.len() > COMPLETE_WORK_SUMMARY_MAX_BYTES)
    {
        return Err(EvidenceScopeError::EvidenceCorrupt(
            "completion summary exceeds bound".into(),
        ));
    }
    if let Some(path) = intent.report_file.as_deref() {
        require_canonical_path(path)?;
    }
    Ok(())
}

fn validate_completion_artifact(artifact: &CompletionArtifactV2) -> Result<(), EvidenceScopeError> {
    match artifact {
        CompletionArtifactV2::DocumentSha256 { rel_path, digest } => {
            require_canonical_path(rel_path)?;
            validate_sha256_token(digest, false)
        }
        CompletionArtifactV2::GitHeadV1 { head } => validate_digest_or_git_token("head", head),
    }
}

fn binding_semantically_matches(
    evidence: &CompletionEvidenceBindingV2,
    current: &CompletionEvidenceBindingV2,
) -> bool {
    evidence.workflow_id == current.workflow_id
        && evidence.task_id == current.task_id
        && evidence.node_id == current.node_id
        && evidence.role == current.role
        && evidence.phase_id == current.phase_id
        && evidence.task_index == current.task_index
        && evidence.gate_id == current.gate_id
        && evidence.gate_lineage == current.gate_lineage
        && evidence.review_round == current.review_round
        && evidence.reviewed_task_id == current.reviewed_task_id
        && evidence.reviewed_generation == current.reviewed_generation
}

fn scope_matches_binding(
    scope: &EvidenceScopeInputV2,
    binding: &CompletionEvidenceBindingV2,
) -> bool {
    scope.workflow_id == binding.workflow_id
        && scope.node.node_id == binding.node_id
        && scope.node.role == binding.role
        && scope.node.phase_id == binding.phase_id
        && scope.node.task_index == binding.task_index
        && scope.gate_id == binding.gate_id
        && scope.gate_lineage == binding.gate_lineage
        && scope.review_round == binding.review_round
        && scope
            .reviewed_producer
            .as_ref()
            .map(|producer| (&producer.task_id, producer.generation))
            == binding
                .reviewed_task_id
                .as_ref()
                .zip(binding.reviewed_generation)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde::Deserialize;
    use serde_json::{json, Value};

    use super::*;
    use crate::acp::delegation::workflow::completion_intent::{
        CompletionIntent, CompletionIntentSource, CompletionOutcome,
    };
    use crate::acp::delegation::workflow::plan_material::{
        derive_holistic_full_cohort_selector, parse_plan_material,
    };
    use crate::acp::delegation::workflow::types::{
        ArtifactSubjectIdentityV2, CompletionArtifactV2, CompletionEvidenceBindingV2,
        CompletionScopeRole, DocumentRef, ManifestTaskPolicy, ManifestTaskRisk, ManifestTaskRoute,
        NormalizedManifest, ReviewedProducerIdentityV2, StableNodeIdentityV2, TaskRiskLevel,
        COMPLETION_PROTOCOL_VERSION_V2, EVIDENCE_SCOPE_SCHEMA_VERSION_V2, MANIFEST_SCHEMA_VERSION,
        TASK_RISK_POLICY_VERSION, WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY,
    };

    #[derive(Debug, Deserialize)]
    struct ScopeVectors {
        schema: String,
        vectors: Vec<ScopeVector>,
    }

    #[derive(Debug, Deserialize)]
    struct ScopeVector {
        name: String,
        domain: String,
        schema_version: u32,
        input: Value,
        canonical_utf8: String,
        sha256: String,
    }

    fn scope_vectors() -> ScopeVectors {
        serde_json::from_str(include_str!("fixtures/completion_scope_vectors.json"))
            .expect("completion scope vectors must parse")
    }

    #[test]
    fn every_scope_consumer_accepts_the_same_golden_vectors() {
        let fixture = scope_vectors();
        assert_eq!(fixture.schema, "CompletionScopeVectorsV1");
        assert!(fixture.vectors.len() >= 15);
        for vector in fixture.vectors {
            assert_eq!(
                canonical_json_bytes(&vector.input).unwrap(),
                vector.canonical_utf8.as_bytes(),
                "canonical bytes for {}",
                vector.name
            );
            assert_eq!(
                canonical_json_sha256(&vector.domain, vector.schema_version, &vector.input)
                    .unwrap(),
                vector.sha256,
                "digest for {}",
                vector.name
            );
            assert!(
                validate_digest_domain(&vector.domain, vector.schema_version, &vector.sha256)
                    .is_ok()
            );
            assert!(validate_digest_domain(
                "codeg.other.v1",
                vector.schema_version,
                &vector.sha256
            )
            .is_err());
        }
    }

    #[test]
    fn every_role_scope_vector_matches_the_typed_production_schema() {
        let role_vector_names = [
            "design_root_scope",
            "design_reviewer_scope",
            "plan_author_scope",
            "plan_reviewer_scope",
            "task_implementer_scope",
            "task_reviewer_scope",
            "final_fixer_scope",
            "final_reviewer_scope",
        ];
        for vector in scope_vectors()
            .vectors
            .into_iter()
            .filter(|vector| role_vector_names.contains(&vector.name.as_str()))
        {
            let instruction_digest = vector.input["instruction_block_digest"]
                .as_str()
                .expect("role vector carries instruction digest");
            let role_scope: RoleReviewScopeV2 =
                serde_json::from_value(vector.input["role_scope"].clone())
                    .expect("role vector uses the production tagged schema");
            let instruction = InstructionBlockV1 {
                template_id: INSTRUCTION_TEMPLATE_ID.into(),
                template_version: INSTRUCTION_TEMPLATE_VERSION,
                canonical_utf8: "{}".into(),
                digest: instruction_digest.into(),
            };
            assert_eq!(
                review_scope_digest(&role_scope, &instruction).unwrap(),
                vector.sha256,
                "typed role digest for {}",
                vector.name
            );
        }
    }

    #[test]
    fn every_completion_role_instruction_matches_fixed_production_vectors() {
        let fixtures = scope_vectors().vectors;
        let cases = [
            (
                "instruction_reviewer",
                CompletionRole::Reviewer,
                PHASE_PLAN,
                None,
                Some("plan"),
                Some(2),
                "task.1",
            ),
            (
                "instruction_author",
                CompletionRole::Author,
                PHASE_PLAN,
                None,
                None,
                None,
                "requirements",
            ),
            (
                "instruction_implementer",
                CompletionRole::Implementer,
                PHASE_TASKS,
                Some(9),
                None,
                None,
                "task.9",
            ),
            (
                "instruction_fixer",
                CompletionRole::Fixer,
                PHASE_FINAL,
                None,
                None,
                None,
                "final.findings",
            ),
        ];
        for (name, role, phase_id, task_index, gate_id, review_round, material_key) in cases {
            let vector = fixtures
                .iter()
                .find(|vector| vector.name == name)
                .expect("production instruction vector is committed");
            let identities = vec![MaterialIdentitySummary {
                key: material_key.into(),
                body_sha256: digest('a'),
            }];
            let instruction = build_instruction_block(&InstructionBlockInput {
                role,
                phase_id,
                task_index,
                gate_id,
                review_round,
                material_identities: &identities,
            })
            .unwrap();
            assert_eq!(instruction.canonical_utf8, vector.canonical_utf8, "{name}");
            assert_eq!(instruction.digest, vector.sha256, "{name}");
        }
    }

    fn digest(ch: char) -> String {
        format!("sha256:{}", ch.to_string().repeat(64))
    }

    fn scope_fixture() -> EvidenceScopeInputV2 {
        EvidenceScopeInputV2 {
            completion_protocol_version: COMPLETION_PROTOCOL_VERSION_V2,
            scope_schema_version: EVIDENCE_SCOPE_SCHEMA_VERSION_V2,
            workflow_id: "wf-1".into(),
            node: StableNodeIdentityV2 {
                node_id: "task-9-impl".into(),
                role: CompletionRole::Implementer,
                phase_id: "tasks".into(),
                task_index: Some(9),
                agent_type: "codex".into(),
                profile_id: None,
                work_unit_key: "task|9|implementer|codex|none".into(),
            },
            gate_id: None,
            gate_lineage: None,
            review_round: None,
            artifact_subject: ArtifactSubjectIdentityV2::GitHeadV1 {
                digest: digest('a'),
            },
            reviewed_producer: None,
            instruction_block_digest: digest('b'),
            review_scope_digest: digest('c'),
        }
    }

    #[test]
    fn canonicalizer_rejects_unsupported_or_noncanonical_values() {
        struct DuplicateKeys;

        impl Serialize for DuplicateKeys {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                use serde::ser::SerializeMap;

                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("value", &1)?;
                map.serialize_entry("value", &2)?;
                map.end()
            }
        }

        assert!(canonical_json_bytes(&json!({"value": 1.5})).is_err());
        assert!(canonical_json_bytes(&DuplicateKeys).is_err());
        assert!(canonical_json_bytes(&json!({"report_file": "../escape.md"})).is_err());
        assert!(
            canonical_json_bytes(&json!({"digest": format!("sha256:{}", "A".repeat(64))})).is_err()
        );
        assert!(canonical_json_sha256("codeg.completion.requirements.v1", 2, &json!({})).is_err());
    }

    #[test]
    fn canonicalizer_rejects_all_f32_values_before_json_serialization() {
        let cases = [
            ("finite", 1.5_f32),
            ("nan", f32::NAN),
            ("positive_infinity", f32::INFINITY),
            ("negative_infinity", f32::NEG_INFINITY),
        ];
        let accepted: Vec<_> = cases
            .iter()
            .filter_map(|(name, value)| canonical_json_bytes(value).ok().map(|_| *name))
            .collect();
        assert!(accepted.is_empty(), "accepted f32 cases: {accepted:?}");
        for (name, value) in cases {
            let error = canonical_json_bytes(&value).expect_err(name);
            assert!(
                error
                    .to_string()
                    .contains("floating-point values are forbidden"),
                "{name}: {error}"
            );
        }
    }

    #[test]
    fn canonicalizer_rejects_all_f64_values_before_json_serialization() {
        let cases = [
            ("finite", 1.5_f64),
            ("nan", f64::NAN),
            ("positive_infinity", f64::INFINITY),
            ("negative_infinity", f64::NEG_INFINITY),
        ];
        let accepted: Vec<_> = cases
            .iter()
            .filter_map(|(name, value)| canonical_json_bytes(value).ok().map(|_| *name))
            .collect();
        assert!(accepted.is_empty(), "accepted f64 cases: {accepted:?}");
        for (name, value) in cases {
            let error = canonical_json_bytes(&value).expect_err(name);
            assert!(
                error
                    .to_string()
                    .contains("floating-point values are forbidden"),
                "{name}: {error}"
            );
        }
    }

    #[test]
    fn canonicalizer_rejects_nested_non_finite_floats_before_json_serialization() {
        #[derive(Serialize)]
        struct NestedFloats {
            f32_value: f32,
            f64_value: Vec<f64>,
        }

        let error = canonical_json_bytes(&NestedFloats {
            f32_value: f32::INFINITY,
            f64_value: vec![f64::NAN],
        })
        .expect_err("nested non-finite floats must be rejected");
        assert!(
            error
                .to_string()
                .contains("floating-point values are forbidden"),
            "{error}"
        );
    }

    #[test]
    fn design_root_builder_binds_only_the_durable_self_review_policy() {
        let scope = build_design_root_review_scope(&DesignRootScopeInput {
            workflow_kind: WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY,
            design: &DocumentRef {
                rel_path: "docs/design.md".into(),
                digest: digest('a'),
            },
            gate_id: "design",
            gate_lineage: &digest('b'),
            resolution_mode: super::super::types::ResolutionMode::SelfReview,
        })
        .unwrap();
        assert_eq!(
            scope,
            RoleReviewScopeV2::DesignRoot {
                workflow_kind: WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY.into(),
                design: DocumentRef {
                    rel_path: "docs/design.md".into(),
                    digest: digest('a'),
                },
                gate_lineage: digest('b'),
                policy_digest:
                    "sha256:89673583571d831948c91c52de5053eb26dcf3e5883c2e3af53b40b6e54691bb".into(),
            }
        );

        assert!(build_design_root_review_scope(&DesignRootScopeInput {
            workflow_kind: WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY,
            design: &DocumentRef {
                rel_path: "docs/design.md".into(),
                digest: digest('a'),
            },
            gate_id: "design",
            gate_lineage: &digest('b'),
            resolution_mode: super::super::types::ResolutionMode::ParentAdjudication,
        })
        .is_err());
    }

    #[test]
    fn non_material_revisions_do_not_change_v2_scope() {
        let scope = scope_fixture();
        let base = AdmissionCompletionContextV2 {
            scope_role: CompletionScopeRole::TaskImplementer,
            instruction: InstructionBlockV1 {
                template_id: "workflow_completion".into(),
                template_version: 1,
                canonical_utf8: "{}".into(),
                digest: scope.instruction_block_digest.clone(),
            },
            review_scope: RoleReviewScopeV2::TaskImplementer {
                task_specification_identity: digest('d'),
                dependency_identities: vec![],
                route_digest: digest('e'),
                admitted_plan_identity: digest('f'),
            },
            review_scope_digest: scope.review_scope_digest.clone(),
            evidence_scope: scope,
            evidence_scope_digest: digest('0'),
            material_selector_digest: None,
            subject_material_digest: None,
            requirements_identity: None,
            task_specification_identity: Some(digest('d')),
            final_findings_identity: None,
            manifest_revision_observed: 1,
            graph_revision_observed: 1,
            required_reviewer_node_ids: vec![],
            display_title: None,
            legacy_content_fingerprint: None,
        };
        let mut changed = base.clone();
        changed.manifest_revision_observed += 1;
        changed.graph_revision_observed += 1;
        changed.required_reviewer_node_ids.push("sibling".into());
        changed.display_title = Some("renamed".into());
        changed.legacy_content_fingerprint = Some("legacy-changed".into());
        assert_eq!(
            admitted_evidence_scope_digest(&base).unwrap(),
            admitted_evidence_scope_digest(&changed).unwrap()
        );
    }

    #[test]
    fn every_material_dimension_changes_scope() {
        let base = scope_fixture();
        let base_digest = evidence_scope_digest(&base).unwrap();
        let mut changed = Vec::new();
        let mut value = base.clone();
        value.workflow_id = "wf-2".into();
        changed.push(value);
        let mut value = base.clone();
        value.node.node_id = "other".into();
        changed.push(value);
        let mut value = base.clone();
        value.node.role = CompletionRole::Fixer;
        changed.push(value);
        let mut value = base.clone();
        value.node.task_index = Some(10);
        changed.push(value);
        let mut value = base.clone();
        value.gate_id = Some("task".into());
        changed.push(value);
        let mut value = base.clone();
        value.gate_lineage = Some(digest('d'));
        changed.push(value);
        let mut value = base.clone();
        value.artifact_subject = ArtifactSubjectIdentityV2::GitHeadV1 {
            digest: digest('d'),
        };
        changed.push(value);
        let mut value = base.clone();
        value.reviewed_producer = Some(ReviewedProducerIdentityV2 {
            task_id: "producer".into(),
            generation: 1,
        });
        changed.push(value);
        let mut value = base.clone();
        value.instruction_block_digest = digest('d');
        changed.push(value);
        let mut value = base.clone();
        value.review_scope_digest = digest('d');
        changed.push(value);
        for value in changed {
            assert_ne!(base_digest, evidence_scope_digest(&value).unwrap());
        }

        let mut first_round = base.clone();
        first_round.node.role = CompletionRole::Reviewer;
        first_round.node.phase_id = "plan".into();
        first_round.node.task_index = None;
        first_round.node.work_unit_key = "plan|docs/plan.md|reviewer|codex|none".into();
        first_round.gate_id = Some("plan".into());
        first_round.gate_lineage = Some(digest('e'));
        first_round.review_round = Some(1);
        let mut second_round = first_round.clone();
        second_round.review_round = Some(2);
        assert_ne!(
            evidence_scope_digest(&first_round).unwrap(),
            evidence_scope_digest(&second_round).unwrap()
        );
    }

    fn normalized_manifest() -> NormalizedManifest {
        NormalizedManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            workflow_kind: WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY.into(),
            plan_target_rel_path: "docs/plan.md".into(),
            risk_policy_version: TASK_RISK_POLICY_VERSION.into(),
            workflow_id: Some("wf-1".into()),
            expected_manifest_revision: Some(1),
            publication_token: "token".into(),
            workflow_state: super::super::types::ManifestWorkflowState::Approved,
            design: Some(DocumentRef {
                rel_path: "docs/design.md".into(),
                digest: digest('a'),
            }),
            plan: Some(DocumentRef {
                rel_path: "docs/plan.md".into(),
                digest: digest('b'),
            }),
            phases: vec![],
            nodes: vec![],
            edges: vec![],
            gates: vec![],
            task_policies: vec![ManifestTaskPolicy {
                task_index: 9,
                risk: ManifestTaskRisk {
                    level: TaskRiskLevel::Normal,
                    hard_triggers: vec![],
                    soft_signals: vec![],
                    score: 0,
                    reason: "fixture".into(),
                },
                route: ManifestTaskRoute {
                    implementer_node_id: "task-9-impl".into(),
                    reviewer_node_ids: vec![],
                },
                allow_noop_verification: false,
            }],
            task_count: 1,
        }
    }

    #[test]
    fn material_identities_are_selected_sorted_and_bound() {
        let material = parse_plan_material(
            b"## Global Constraints\n\n- exact\n\n## Task 9: Build\n\nbody\n",
            &[9],
        )
        .unwrap();
        let bound = material
            .with_manifest_policies(&normalized_manifest())
            .unwrap();
        let selector = derive_holistic_full_cohort_selector(&bound);
        let summaries = material_identity_summaries(&material, &selector).unwrap();
        assert_eq!(summaries.len(), 5);
        assert!(summaries.windows(2).all(|pair| pair[0].key < pair[1].key));
        assert_eq!(
            summaries
                .iter()
                .map(|item| item.key.as_str())
                .collect::<BTreeSet<_>>(),
            material.keys().map(String::as_str).collect()
        );
    }

    #[test]
    fn task_route_scope_excludes_sibling_reviewer_roster() {
        let base = normalized_manifest();
        let mut changed = base.clone();
        changed.task_policies[0].route.reviewer_node_ids = vec!["replacement-reviewer".into()];

        assert_eq!(
            task_route_digest(&base, 9).unwrap(),
            task_route_digest(&changed, 9).unwrap()
        );
    }

    #[test]
    fn plan_reviewer_policy_excludes_equivalent_route_node_ids() {
        use crate::acp::delegation::workflow::types::{
            ManifestNodeKind, ManifestNodeRole, NormalizedNode,
        };

        let route_reviewer = |id: &str| NormalizedNode {
            id: id.into(),
            kind: ManifestNodeKind::WorkUnit,
            phase_id: Some(PHASE_TASKS.into()),
            role: Some(ManifestNodeRole::Reviewer),
            agent_type: Some("codex".into()),
            profile_id: None,
            task_index: Some(9),
            work_unit_key: Some("task|9|reviewer|codex|none".to_string()),
            deps: vec![],
            required: true,
            node_outcome: None,
            title: None,
        };
        let reviewer = delegation_workflow_node_binding::Model {
            workflow_id: "wf-1".into(),
            node_id: "plan-reviewer".into(),
            work_unit_key: "plan|docs/plan.md|reviewer|codex|none".into(),
            role: "reviewer".into(),
            agent_type: "codex".into(),
            profile_id: None,
            phase_id: PHASE_PLAN.into(),
            task_index: None,
            introduced_revision: 1,
            retired_revision: None,
            is_observed: false,
            retained_observed: false,
            cohort_frozen: false,
            node_outcome: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let mut base = normalized_manifest();
        base.nodes = vec![route_reviewer("task-reviewer-a")];
        base.task_policies[0].route.reviewer_node_ids = vec!["task-reviewer-a".into()];
        let mut changed = base.clone();
        changed.nodes = vec![route_reviewer("task-reviewer-b")];
        changed.task_policies[0].route.reviewer_node_ids = vec!["task-reviewer-b".into()];

        assert_eq!(
            plan_reviewer_policy_digest(&base, &reviewer, Some("plan")).unwrap(),
            plan_reviewer_policy_digest(&changed, &reviewer, Some("plan")).unwrap()
        );
    }

    #[test]
    fn final_review_requirements_exclude_sibling_nodes_and_edges() {
        use crate::acp::delegation::workflow::types::{
            ManifestEdge, ManifestNodeKind, ManifestNodeRole, NormalizedNode,
        };

        let final_node = |id: &str, role: ManifestNodeRole| NormalizedNode {
            id: id.into(),
            kind: ManifestNodeKind::WorkUnit,
            phase_id: Some(PHASE_FINAL.into()),
            role: Some(role),
            agent_type: Some("codex".into()),
            profile_id: None,
            task_index: None,
            work_unit_key: Some(format!("final|{role:?}|codex|none").to_lowercase()),
            deps: vec![],
            required: true,
            node_outcome: None,
            title: None,
        };
        let mut base = normalized_manifest();
        base.nodes = vec![
            final_node("final-reviewer", ManifestNodeRole::Reviewer),
            final_node("final-fixer-a", ManifestNodeRole::Fixer),
        ];
        base.edges = vec![
            ManifestEdge {
                id: Some("subject-edge".into()),
                from: "task-9-impl".into(),
                to: "final-reviewer".into(),
            },
            ManifestEdge {
                id: Some("sibling-edge".into()),
                from: "task-9-rev".into(),
                to: "final-fixer-a".into(),
            },
        ];
        let mut sibling_changed = base.clone();
        sibling_changed.nodes[1] = final_node("final-fixer-b", ManifestNodeRole::Fixer);
        sibling_changed.edges[1].to = "final-fixer-b".into();
        let mut subject_changed = base.clone();
        subject_changed.edges[0].from = "task-10-impl".into();

        let digest = final_review_requirements_digest(&base, "final-reviewer").unwrap();
        assert_eq!(
            digest,
            final_review_requirements_digest(&sibling_changed, "final-reviewer").unwrap()
        );
        assert_ne!(
            digest,
            final_review_requirements_digest(&subject_changed, "final-reviewer").unwrap()
        );
    }

    #[test]
    fn task_output_identity_binds_selected_run_and_generation() {
        let first =
            task_output_identity_digest(9, "task-9-impl", "producer-run-a", 1, &digest('a'))
                .unwrap();
        let replacement_run =
            task_output_identity_digest(9, "task-9-impl", "producer-run-b", 1, &digest('a'))
                .unwrap();
        let replacement_generation =
            task_output_identity_digest(9, "task-9-impl", "producer-run-a", 2, &digest('a'))
                .unwrap();

        assert_ne!(first, replacement_run);
        assert_ne!(first, replacement_generation);
    }

    #[tokio::test]
    async fn durable_document_loading_is_workspace_contained_and_bounded() {
        let workspace = tempfile::tempdir().unwrap();
        let rel_path = "docs/oversized.md";
        let path = workspace.path().join(rel_path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let bytes = vec![b'x'; super::super::plan_material::MAX_PLAN_MATERIAL_BYTES + 1];
        std::fs::write(&path, &bytes).unwrap();
        let document = DocumentRef {
            rel_path: rel_path.into(),
            digest: sha256_prefixed(&bytes),
        };

        assert!(matches!(
            verified_document(
                workspace.path(),
                &document,
                "Design",
                super::super::plan_material::MAX_PLAN_MATERIAL_BYTES,
            )
            .await,
            Err(EvidenceScopeError::PlanMaterialInvalid(_))
        ));
    }

    #[test]
    fn instruction_block_is_role_bound_bounded_and_domain_separated() {
        let identities = vec![MaterialIdentitySummary {
            key: "task.9".into(),
            body_sha256: digest('a'),
        }];
        let reviewer = build_instruction_block(&InstructionBlockInput {
            role: CompletionRole::Reviewer,
            phase_id: "plan",
            task_index: None,
            gate_id: Some("plan"),
            review_round: Some(2),
            material_identities: &identities,
        })
        .unwrap();
        assert!(reviewer.canonical_utf8.contains("request changes"));
        assert!(!reviewer.canonical_utf8.contains("done with concerns"));
        assert!(reviewer.canonical_utf8.len() <= 64 * 1024);
        assert_eq!(
            reviewer.digest,
            canonical_json_sha256(
                "codeg.completion.instruction.v1",
                1,
                &serde_json::from_str::<Value>(&reviewer.canonical_utf8).unwrap()
            )
            .unwrap()
        );
    }

    #[test]
    fn review_scope_digest_binds_the_exact_instruction_block() {
        let scope = RoleReviewScopeV2::PlanAuthor {
            plan_target_rel_path: "docs/plan.md".into(),
            requirements_identity: digest('a'),
        };
        let first_material = vec![MaterialIdentitySummary {
            key: "requirements".into(),
            body_sha256: digest('a'),
        }];
        let second_material = vec![MaterialIdentitySummary {
            key: "requirements".into(),
            body_sha256: digest('b'),
        }];
        let first = build_instruction_block(&InstructionBlockInput {
            role: CompletionRole::Author,
            phase_id: "plan",
            task_index: None,
            gate_id: None,
            review_round: None,
            material_identities: &first_material,
        })
        .unwrap();
        let second = build_instruction_block(&InstructionBlockInput {
            role: CompletionRole::Author,
            phase_id: "plan",
            task_index: None,
            gate_id: None,
            review_round: None,
            material_identities: &second_material,
        })
        .unwrap();

        assert_ne!(
            review_scope_digest(&scope, &first).unwrap(),
            review_scope_digest(&scope, &second).unwrap()
        );
    }

    fn evidence_fixture() -> (CompletionEvidenceV2, EvidenceValidationContext) {
        let scope = scope_fixture();
        let binding = CompletionEvidenceBindingV2 {
            workflow_id: scope.workflow_id.clone(),
            task_id: "task-run-9".into(),
            node_id: scope.node.node_id.clone(),
            role: scope.node.role,
            phase_id: scope.node.phase_id.clone(),
            task_index: scope.node.task_index,
            gate_id: scope.gate_id.clone(),
            gate_lineage: scope.gate_lineage.clone(),
            review_round: scope.review_round,
            reviewed_task_id: None,
            reviewed_generation: None,
            manifest_revision_observed: 9,
        };
        let artifact = CompletionArtifactV2::GitHeadV1 { head: digest('a') };
        let evidence = CompletionEvidenceV2 {
            version: 2,
            intent: CompletionIntent {
                outcome: CompletionOutcome::Done,
                summary: Some("implemented".into()),
                report_file: Some("reports/task-9.md".into()),
                source: CompletionIntentSource::AssistantConclusion,
            },
            binding: binding.clone(),
            artifact: artifact.clone(),
            review_scope_digest: scope.review_scope_digest.clone(),
            evidence_scope_digest: digest('0'),
            captured_at: "2026-08-05T00:00:00Z".into(),
        };
        let mut evidence = evidence;
        evidence.evidence_scope_digest = evidence_scope_digest(&scope).unwrap();
        (
            evidence,
            EvidenceValidationContext {
                role: CompletionRole::Implementer,
                binding,
                artifact,
                scope,
            },
        )
    }

    #[test]
    fn shared_validator_rejects_unknown_fields_role_mismatch_and_stale_scope() {
        let (evidence, current) = evidence_fixture();
        let json = serde_json::to_string(&evidence).unwrap();
        let validated = validate_completion_evidence(&json, &current).unwrap();
        assert!(validated.evidence_validated);
        assert_eq!(validated.evidence, evidence);

        let mut unknown = serde_json::to_value(&evidence).unwrap();
        unknown
            .as_object_mut()
            .unwrap()
            .insert("card_summary_json".into(), json!({"status": "done"}));
        assert_eq!(
            validate_completion_evidence(&unknown.to_string(), &current)
                .unwrap_err()
                .code(),
            "completion_evidence_corrupt"
        );

        let mut mismatch = evidence.clone();
        mismatch.intent.outcome = CompletionOutcome::Approve;
        assert_eq!(
            validate_completion_evidence(&serde_json::to_string(&mismatch).unwrap(), &current)
                .unwrap_err()
                .code(),
            "completion_outcome_role_mismatch"
        );

        let mut stale = current.clone();
        stale.scope.node.node_id = "replacement".into();
        assert_eq!(
            validate_completion_evidence(&json, &stale)
                .unwrap_err()
                .code(),
            "completion_scope_changed"
        );
    }
}
