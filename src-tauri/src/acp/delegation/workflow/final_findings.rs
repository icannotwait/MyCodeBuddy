use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use sea_orm::{ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Set};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::completion_intent::CompletionOutcome;
use super::evidence_scope::canonical_json_sha256;
use super::key::normalize_rel_path;
use crate::db::entities::delegation_final_findings_package::{self, FinalFindingsPackageStatus};

const FINAL_FINDINGS_DOMAIN: &str = "codeg.completion.final_findings.v1";
const FINAL_FINDINGS_SCHEMA_VERSION: u32 = 1;
const MAX_CONTEXT_BYTES: usize = 512 * 1024;
const MAX_CONTEXT_COUNT: usize = 64;
const MAX_FINDING_COUNT: usize = 64;
const MAX_ID_CHARS: usize = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemediationContextSourceKind {
    ReportFile,
    TerminalSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemediationContextAvailability {
    Available,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemediationContextInputV1 {
    pub source_evidence_task_id: String,
    pub source_kind: RemediationContextSourceKind,
    pub rel_path: Option<String>,
    pub bytes: Option<Vec<u8>>,
}

impl RemediationContextInputV1 {
    pub fn available_report(
        source_evidence_task_id: impl Into<String>,
        rel_path: impl Into<String>,
        bytes: Vec<u8>,
    ) -> Self {
        Self {
            source_evidence_task_id: source_evidence_task_id.into(),
            source_kind: RemediationContextSourceKind::ReportFile,
            rel_path: Some(rel_path.into()),
            bytes: Some(bytes),
        }
    }

    pub fn available_terminal(source_evidence_task_id: impl Into<String>, bytes: Vec<u8>) -> Self {
        Self {
            source_evidence_task_id: source_evidence_task_id.into(),
            source_kind: RemediationContextSourceKind::TerminalSnapshot,
            rel_path: None,
            bytes: Some(bytes),
        }
    }

    pub fn missing_report(
        source_evidence_task_id: impl Into<String>,
        rel_path: impl Into<String>,
    ) -> Self {
        Self {
            source_evidence_task_id: source_evidence_task_id.into(),
            source_kind: RemediationContextSourceKind::ReportFile,
            rel_path: Some(rel_path.into()),
            bytes: None,
        }
    }

    pub fn missing_terminal(source_evidence_task_id: impl Into<String>) -> Self {
        Self {
            source_evidence_task_id: source_evidence_task_id.into(),
            source_kind: RemediationContextSourceKind::TerminalSnapshot,
            rel_path: None,
            bytes: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalFindingInputV1 {
    pub reviewer_node_id: String,
    pub evidence_task_id: String,
    pub evidence_scope_digest: String,
    pub outcome: CompletionOutcome,
    pub target_work_unit_keys: Vec<String>,
    pub remediation_route_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalFindingsPackageInputV1 {
    pub workflow_id: String,
    pub gate_id: String,
    pub gate_lineage: String,
    pub graph_revision: u64,
    pub findings: Vec<FinalFindingInputV1>,
    pub remediation_contexts: Vec<RemediationContextInputV1>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinalFindingItemV1 {
    pub finding_id: String,
    pub reviewer_node_id: String,
    pub evidence_task_id: String,
    pub evidence_scope_digest: String,
    pub outcome: CompletionOutcome,
    pub target_work_unit_keys: Vec<String>,
    pub remediation_route_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemediationContextSnapshotV1 {
    pub source_evidence_task_id: String,
    pub source_kind: RemediationContextSourceKind,
    pub rel_path: Option<String>,
    pub content_sha256: String,
    pub byte_len: u64,
    pub availability: RemediationContextAvailability,
    pub content_base64: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinalFindingsPackageV1 {
    pub workflow_id: String,
    pub gate_id: String,
    pub gate_lineage: String,
    pub source_evaluation_key: String,
    pub items: Vec<FinalFindingItemV1>,
    pub remediation_contexts: Vec<RemediationContextSnapshotV1>,
    pub package_digest: String,
}

impl FinalFindingsPackageV1 {
    pub fn final_findings_identity(&self) -> &str {
        &self.package_digest
    }

    pub fn context_bytes(&self, index: usize) -> Result<Vec<u8>, FinalFindingsError> {
        let context = self
            .remediation_contexts
            .get(index)
            .ok_or(FinalFindingsError::EvidenceCorrupt)?;
        decode_context(context)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FinalFindingsError {
    #[error("Final findings package field is invalid: {0}")]
    InvalidField(String),
    #[error("Final findings package exceeds a bound: {0}")]
    BoundsExceeded(String),
    #[error("completion remediation context is required")]
    RemediationContextRequired,
    #[error("completion evidence is corrupt")]
    EvidenceCorrupt,
    #[error("Final findings persistence failure: {0}")]
    Persistence(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedFinalFindingsPackageV1 {
    pub source_evidence_task_ids_json: String,
    pub items_json: String,
    pub remediation_contexts_json: String,
}

pub fn encode_final_findings_package_v1(
    package: &FinalFindingsPackageV1,
) -> Result<EncodedFinalFindingsPackageV1, FinalFindingsError> {
    verify_final_findings_package_v1(package)?;
    let source_evidence_task_ids = package
        .items
        .iter()
        .map(|item| item.evidence_task_id.as_str())
        .collect::<Vec<_>>();
    Ok(EncodedFinalFindingsPackageV1 {
        source_evidence_task_ids_json: serde_json::to_string(&source_evidence_task_ids)
            .map_err(|error| FinalFindingsError::InvalidField(error.to_string()))?,
        items_json: serde_json::to_string(&package.items)
            .map_err(|error| FinalFindingsError::InvalidField(error.to_string()))?,
        remediation_contexts_json: serde_json::to_string(&package.remediation_contexts)
            .map_err(|error| FinalFindingsError::InvalidField(error.to_string()))?,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn decode_final_findings_package_v1(
    workflow_id: &str,
    gate_id: &str,
    gate_lineage: &str,
    source_evaluation_key: &str,
    items_json: &str,
    remediation_contexts_json: &str,
    package_digest: &str,
) -> Result<FinalFindingsPackageV1, FinalFindingsError> {
    let package = FinalFindingsPackageV1 {
        workflow_id: workflow_id.to_owned(),
        gate_id: gate_id.to_owned(),
        gate_lineage: gate_lineage.to_owned(),
        source_evaluation_key: source_evaluation_key.to_owned(),
        items: serde_json::from_str(items_json).map_err(|_| FinalFindingsError::EvidenceCorrupt)?,
        remediation_contexts: serde_json::from_str(remediation_contexts_json)
            .map_err(|_| FinalFindingsError::EvidenceCorrupt)?,
        package_digest: package_digest.to_owned(),
    };
    verify_final_findings_package_v1(&package)?;
    Ok(package)
}

pub async fn capture_report_context_v1(
    workspace: &Path,
    source_evidence_task_id: &str,
    rel_path: &str,
) -> Result<RemediationContextInputV1, FinalFindingsError> {
    validate_id("source_evidence_task_id", source_evidence_task_id)?;
    let rel_path = normalize_rel_path(rel_path)
        .map_err(|error| FinalFindingsError::InvalidField(error.to_string()))?;
    let extension = Path::new(&rel_path)
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase);
    if !matches!(extension.as_deref(), Some("md" | "markdown")) {
        return Err(FinalFindingsError::InvalidField(
            "remediation report must be Markdown".into(),
        ));
    }
    let workspace = workspace.to_path_buf();
    let read_path = rel_path.clone();
    let bytes = tokio::task::spawn_blocking(move || read_report_bytes(&workspace, &read_path))
        .await
        .map_err(|_| FinalFindingsError::EvidenceCorrupt)??;
    Ok(match bytes {
        Some(bytes) => {
            RemediationContextInputV1::available_report(source_evidence_task_id, rel_path, bytes)
        }
        None => RemediationContextInputV1::missing_report(source_evidence_task_id, rel_path),
    })
}

pub(crate) fn bounded_terminal_context_v1(
    source_evidence_task_id: &str,
    bytes: &[u8],
) -> RemediationContextInputV1 {
    let start = bytes.len().saturating_sub(MAX_CONTEXT_BYTES);
    RemediationContextInputV1::available_terminal(source_evidence_task_id, bytes[start..].to_vec())
}

pub async fn persist_final_findings_package_v1<C: ConnectionTrait>(
    conn: &C,
    package: &FinalFindingsPackageV1,
    graph_revision: i64,
) -> Result<delegation_final_findings_package::Model, FinalFindingsError> {
    if graph_revision <= 0 {
        return Err(FinalFindingsError::InvalidField(
            "graph_revision must be positive".into(),
        ));
    }
    verify_final_findings_package_v1(package)?;
    let active = delegation_final_findings_package::Entity::find()
        .filter(delegation_final_findings_package::Column::WorkflowId.eq(&package.workflow_id))
        .filter(delegation_final_findings_package::Column::GateId.eq(&package.gate_id))
        .filter(
            delegation_final_findings_package::Column::Status
                .eq(FinalFindingsPackageStatus::Active),
        )
        .all(conn)
        .await
        .map_err(|error| FinalFindingsError::Persistence(error.to_string()))?;
    for row in &active {
        if row.gate_lineage == package.gate_lineage && row.package_digest == package.package_digest
        {
            verify_final_findings_package_model_v1(row)?;
            return Ok(row.clone());
        }
    }
    for row in active {
        let mut row: delegation_final_findings_package::ActiveModel = row.into();
        row.status = Set(FinalFindingsPackageStatus::Superseded);
        row.resolved_graph_revision = Set(Some(graph_revision));
        row.update(conn)
            .await
            .map_err(|error| FinalFindingsError::Persistence(error.to_string()))?;
    }

    let encoded = encode_final_findings_package_v1(package)?;
    delegation_final_findings_package::ActiveModel {
        package_id: Set(format!("final-package:{}", &package.package_digest[7..])),
        workflow_id: Set(package.workflow_id.clone()),
        gate_id: Set(package.gate_id.clone()),
        gate_lineage: Set(package.gate_lineage.clone()),
        source_evaluation_key: Set(package.source_evaluation_key.clone()),
        source_evidence_task_ids_json: Set(encoded.source_evidence_task_ids_json),
        items_json: Set(encoded.items_json),
        remediation_contexts_json: Set(encoded.remediation_contexts_json),
        package_digest: Set(package.package_digest.clone()),
        status: Set(FinalFindingsPackageStatus::Active),
        created_graph_revision: Set(graph_revision),
        resolved_graph_revision: Set(None),
    }
    .insert(conn)
    .await
    .map_err(|error| FinalFindingsError::Persistence(error.to_string()))
}

pub async fn load_active_final_findings_package_v1<C: ConnectionTrait>(
    conn: &C,
    workflow_id: &str,
    gate_id: &str,
    gate_lineage: &str,
) -> Result<Option<FinalFindingsPackageV1>, FinalFindingsError> {
    let row = delegation_final_findings_package::Entity::find()
        .filter(delegation_final_findings_package::Column::WorkflowId.eq(workflow_id))
        .filter(delegation_final_findings_package::Column::GateId.eq(gate_id))
        .filter(delegation_final_findings_package::Column::GateLineage.eq(gate_lineage))
        .filter(
            delegation_final_findings_package::Column::Status
                .eq(FinalFindingsPackageStatus::Active),
        )
        .one(conn)
        .await
        .map_err(|error| FinalFindingsError::Persistence(error.to_string()))?;
    row.as_ref()
        .map(verify_final_findings_package_model_v1)
        .transpose()
}

pub async fn resolve_active_final_findings_packages_v1<C: ConnectionTrait>(
    conn: &C,
    workflow_id: &str,
    gate_id: &str,
    graph_revision: i64,
) -> Result<u64, FinalFindingsError> {
    if graph_revision <= 0 {
        return Err(FinalFindingsError::InvalidField(
            "graph_revision must be positive".into(),
        ));
    }
    let active = delegation_final_findings_package::Entity::find()
        .filter(delegation_final_findings_package::Column::WorkflowId.eq(workflow_id))
        .filter(delegation_final_findings_package::Column::GateId.eq(gate_id))
        .filter(
            delegation_final_findings_package::Column::Status
                .eq(FinalFindingsPackageStatus::Active),
        )
        .all(conn)
        .await
        .map_err(|error| FinalFindingsError::Persistence(error.to_string()))?;
    let count = active.len() as u64;
    for row in active {
        let mut row: delegation_final_findings_package::ActiveModel = row.into();
        row.status = Set(FinalFindingsPackageStatus::Resolved);
        row.resolved_graph_revision = Set(Some(graph_revision));
        row.update(conn)
            .await
            .map_err(|error| FinalFindingsError::Persistence(error.to_string()))?;
    }
    Ok(count)
}

pub fn verify_final_findings_package_model_v1(
    row: &delegation_final_findings_package::Model,
) -> Result<FinalFindingsPackageV1, FinalFindingsError> {
    let package = decode_final_findings_package_v1(
        &row.workflow_id,
        &row.gate_id,
        &row.gate_lineage,
        &row.source_evaluation_key,
        &row.items_json,
        &row.remediation_contexts_json,
        &row.package_digest,
    )?;
    let encoded = encode_final_findings_package_v1(&package)?;
    if encoded.source_evidence_task_ids_json != row.source_evidence_task_ids_json {
        return Err(FinalFindingsError::EvidenceCorrupt);
    }
    Ok(package)
}

fn read_report_bytes(
    workspace: &Path,
    rel_path: &str,
) -> Result<Option<Vec<u8>>, FinalFindingsError> {
    let workspace = std::fs::canonicalize(workspace)
        .map_err(|_| FinalFindingsError::InvalidField("workspace is unavailable".into()))?;
    let candidate: PathBuf = workspace.join(rel_path);
    let target = match std::fs::canonicalize(&candidate) {
        Ok(target) => target,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(FinalFindingsError::EvidenceCorrupt),
    };
    if !target.starts_with(&workspace) {
        return Err(FinalFindingsError::InvalidField(
            "remediation report escapes workspace".into(),
        ));
    }
    let metadata = std::fs::metadata(&target).map_err(|_| FinalFindingsError::EvidenceCorrupt)?;
    if !metadata.is_file() {
        return Err(FinalFindingsError::InvalidField(
            "remediation report is not a file".into(),
        ));
    }
    if metadata.len() > MAX_CONTEXT_BYTES as u64 {
        return Err(FinalFindingsError::BoundsExceeded(format!(
            "context bytes exceed {MAX_CONTEXT_BYTES}"
        )));
    }
    let bytes = std::fs::read(target).map_err(|_| FinalFindingsError::EvidenceCorrupt)?;
    if bytes.len() > MAX_CONTEXT_BYTES {
        return Err(FinalFindingsError::BoundsExceeded(format!(
            "context bytes exceed {MAX_CONTEXT_BYTES}"
        )));
    }
    Ok(Some(bytes))
}

pub fn build_final_findings_package_v1(
    input: FinalFindingsPackageInputV1,
) -> Result<FinalFindingsPackageV1, FinalFindingsError> {
    validate_id("workflow_id", &input.workflow_id)?;
    validate_id("gate_id", &input.gate_id)?;
    validate_id("gate_lineage", &input.gate_lineage)?;
    if input.graph_revision == 0 {
        return Err(FinalFindingsError::InvalidField(
            "graph_revision must be positive".into(),
        ));
    }
    if input.findings.is_empty() || input.findings.len() > MAX_FINDING_COUNT {
        return Err(FinalFindingsError::BoundsExceeded(format!(
            "finding count must be 1..={MAX_FINDING_COUNT}"
        )));
    }
    if input.remediation_contexts.len() > MAX_CONTEXT_COUNT {
        return Err(FinalFindingsError::BoundsExceeded(format!(
            "context count exceeds {MAX_CONTEXT_COUNT}"
        )));
    }

    let mut items = input
        .findings
        .into_iter()
        .map(|finding| build_item(&input.gate_lineage, finding))
        .collect::<Result<Vec<_>, _>>()?;
    items.sort_by(|left, right| left.finding_id.cmp(&right.finding_id));
    if items
        .windows(2)
        .any(|pair| pair[0].finding_id == pair[1].finding_id)
    {
        return Err(FinalFindingsError::InvalidField(
            "duplicate Final reviewer evidence".into(),
        ));
    }

    let remediation_contexts = input
        .remediation_contexts
        .into_iter()
        .map(build_context)
        .collect::<Result<Vec<_>, _>>()?;
    if !remediation_contexts.iter().any(|context| {
        context.availability == RemediationContextAvailability::Available && context.byte_len > 0
    }) {
        return Err(FinalFindingsError::RemediationContextRequired);
    }
    let source_evaluation_key = canonical_hash(&json!({
        "kind": "source_evaluation",
        "workflow_id": input.workflow_id,
        "gate_id": input.gate_id,
        "gate_lineage": input.gate_lineage,
        "graph_revision": input.graph_revision,
        "reviewers": items.iter().map(|item| json!({
            "finding_id": item.finding_id,
            "reviewer_node_id": item.reviewer_node_id,
            "evidence_task_id": item.evidence_task_id,
            "evidence_scope_digest": item.evidence_scope_digest,
            "outcome": item.outcome,
            "target_work_unit_keys": item.target_work_unit_keys,
            "remediation_route_ids": item.remediation_route_ids,
        })).collect::<Vec<_>>(),
    }))?;
    let package_digest = package_digest(
        &input.workflow_id,
        &input.gate_id,
        &input.gate_lineage,
        &source_evaluation_key,
        &items,
        &remediation_contexts,
    )?;
    let package = FinalFindingsPackageV1 {
        workflow_id: input.workflow_id,
        gate_id: input.gate_id,
        gate_lineage: input.gate_lineage,
        source_evaluation_key,
        items,
        remediation_contexts,
        package_digest,
    };
    verify_final_findings_package_v1(&package)?;
    Ok(package)
}

pub fn verify_final_findings_package_v1(
    package: &FinalFindingsPackageV1,
) -> Result<(), FinalFindingsError> {
    validate_id("workflow_id", &package.workflow_id)?;
    validate_id("gate_id", &package.gate_id)?;
    validate_id("gate_lineage", &package.gate_lineage)?;
    validate_sha256("source_evaluation_key", &package.source_evaluation_key)?;
    validate_sha256("package_digest", &package.package_digest)?;
    if package.items.is_empty()
        || package.items.len() > MAX_FINDING_COUNT
        || package.remediation_contexts.len() > MAX_CONTEXT_COUNT
        || package
            .items
            .windows(2)
            .any(|pair| pair[0].finding_id >= pair[1].finding_id)
    {
        return Err(FinalFindingsError::EvidenceCorrupt);
    }
    for context in &package.remediation_contexts {
        decode_context(context)?;
    }
    if !package.remediation_contexts.iter().any(|context| {
        context.availability == RemediationContextAvailability::Available && context.byte_len > 0
    }) {
        return Err(FinalFindingsError::RemediationContextRequired);
    }
    let expected = package_digest(
        &package.workflow_id,
        &package.gate_id,
        &package.gate_lineage,
        &package.source_evaluation_key,
        &package.items,
        &package.remediation_contexts,
    )?;
    if expected != package.package_digest {
        return Err(FinalFindingsError::EvidenceCorrupt);
    }
    Ok(())
}

fn build_item(
    gate_lineage: &str,
    finding: FinalFindingInputV1,
) -> Result<FinalFindingItemV1, FinalFindingsError> {
    validate_id("reviewer_node_id", &finding.reviewer_node_id)?;
    validate_id("evidence_task_id", &finding.evidence_task_id)?;
    validate_sha256("evidence_scope_digest", &finding.evidence_scope_digest)?;
    if !matches!(
        finding.outcome,
        CompletionOutcome::RequestChanges | CompletionOutcome::Block
    ) {
        return Err(FinalFindingsError::InvalidField(
            "Final findings require a non-passing Reviewer outcome".into(),
        ));
    }
    let target_work_unit_keys =
        canonical_ids("target_work_unit_keys", finding.target_work_unit_keys)?;
    let remediation_route_ids =
        canonical_ids("remediation_route_ids", finding.remediation_route_ids)?;
    let finding_id = sha256_token(
        format!(
            "codeg.final-finding.v1\0{gate_lineage}\0{}\0{}",
            finding.reviewer_node_id, finding.evidence_task_id
        )
        .as_bytes(),
    );
    Ok(FinalFindingItemV1 {
        finding_id,
        reviewer_node_id: finding.reviewer_node_id,
        evidence_task_id: finding.evidence_task_id,
        evidence_scope_digest: finding.evidence_scope_digest,
        outcome: finding.outcome,
        target_work_unit_keys,
        remediation_route_ids,
    })
}

fn build_context(
    context: RemediationContextInputV1,
) -> Result<RemediationContextSnapshotV1, FinalFindingsError> {
    validate_id("source_evidence_task_id", &context.source_evidence_task_id)?;
    let rel_path = match (context.source_kind, context.rel_path) {
        (RemediationContextSourceKind::ReportFile, Some(path)) => Some(
            normalize_rel_path(&path)
                .map_err(|error| FinalFindingsError::InvalidField(error.to_string()))?,
        ),
        (RemediationContextSourceKind::ReportFile, None) => {
            return Err(FinalFindingsError::InvalidField(
                "report context requires rel_path".into(),
            ));
        }
        (RemediationContextSourceKind::TerminalSnapshot, None) => None,
        (RemediationContextSourceKind::TerminalSnapshot, Some(_)) => {
            return Err(FinalFindingsError::InvalidField(
                "terminal context cannot carry rel_path".into(),
            ));
        }
    };
    let (content_sha256, byte_len, availability, content_base64) = match context.bytes {
        Some(bytes) => {
            if bytes.len() > MAX_CONTEXT_BYTES {
                return Err(FinalFindingsError::BoundsExceeded(format!(
                    "context bytes exceed {MAX_CONTEXT_BYTES}"
                )));
            }
            (
                sha256_token(&bytes),
                bytes.len() as u64,
                RemediationContextAvailability::Available,
                Some(BASE64_STANDARD.encode(bytes)),
            )
        }
        None => (
            sha256_token(&[]),
            0,
            RemediationContextAvailability::Missing,
            None,
        ),
    };
    Ok(RemediationContextSnapshotV1 {
        source_evidence_task_id: context.source_evidence_task_id,
        source_kind: context.source_kind,
        rel_path,
        content_sha256,
        byte_len,
        availability,
        content_base64,
    })
}

fn decode_context(context: &RemediationContextSnapshotV1) -> Result<Vec<u8>, FinalFindingsError> {
    validate_id("source_evidence_task_id", &context.source_evidence_task_id)?;
    validate_sha256("content_sha256", &context.content_sha256)?;
    match context.availability {
        RemediationContextAvailability::Missing => {
            if context.byte_len != 0
                || context.content_base64.is_some()
                || context.content_sha256 != sha256_token(&[])
            {
                return Err(FinalFindingsError::EvidenceCorrupt);
            }
            Ok(Vec::new())
        }
        RemediationContextAvailability::Available => {
            let encoded = context
                .content_base64
                .as_deref()
                .ok_or(FinalFindingsError::EvidenceCorrupt)?;
            let bytes = BASE64_STANDARD
                .decode(encoded)
                .map_err(|_| FinalFindingsError::EvidenceCorrupt)?;
            if bytes.len() > MAX_CONTEXT_BYTES
                || u64::try_from(bytes.len()).ok() != Some(context.byte_len)
                || sha256_token(&bytes) != context.content_sha256
            {
                return Err(FinalFindingsError::EvidenceCorrupt);
            }
            Ok(bytes)
        }
    }
}

fn package_digest(
    workflow_id: &str,
    gate_id: &str,
    gate_lineage: &str,
    source_evaluation_key: &str,
    items: &[FinalFindingItemV1],
    remediation_contexts: &[RemediationContextSnapshotV1],
) -> Result<String, FinalFindingsError> {
    canonical_hash(&json!({
        "kind": "package",
        "workflow_id": workflow_id,
        "gate_id": gate_id,
        "gate_lineage": gate_lineage,
        "source_evaluation_key": source_evaluation_key,
        "items": items,
        "remediation_contexts": remediation_contexts.iter().map(|context| json!({
            "source_evidence_task_id": context.source_evidence_task_id,
            "source_kind": context.source_kind,
            "rel_path": context.rel_path,
            "content_sha256": context.content_sha256,
            "byte_len": context.byte_len,
            "availability": context.availability,
        })).collect::<Vec<_>>(),
    }))
}

fn canonical_hash(value: &serde_json::Value) -> Result<String, FinalFindingsError> {
    canonical_json_sha256(FINAL_FINDINGS_DOMAIN, FINAL_FINDINGS_SCHEMA_VERSION, value)
        .map_err(|error| FinalFindingsError::InvalidField(error.to_string()))
}

fn canonical_ids(field: &str, values: Vec<String>) -> Result<Vec<String>, FinalFindingsError> {
    let values = values.into_iter().collect::<BTreeSet<_>>();
    for value in &values {
        validate_id(field, value)?;
    }
    Ok(values.into_iter().collect())
}

fn validate_id(field: &str, value: &str) -> Result<(), FinalFindingsError> {
    if value.trim().is_empty() || value.chars().count() > MAX_ID_CHARS {
        return Err(FinalFindingsError::InvalidField(field.into()));
    }
    Ok(())
}

fn validate_sha256(field: &str, value: &str) -> Result<(), FinalFindingsError> {
    if value.len() != 71
        || !value.starts_with("sha256:")
        || !value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(FinalFindingsError::InvalidField(field.into()));
    }
    Ok(())
}

fn sha256_token(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::delegation::workflow::CompletionOutcome;

    const LINEAGE: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const SCOPE_A: &str = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
    const SCOPE_B: &str = "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

    fn finding(
        reviewer_node_id: &str,
        evidence_task_id: &str,
        evidence_scope_digest: &str,
        outcome: CompletionOutcome,
    ) -> FinalFindingInputV1 {
        FinalFindingInputV1 {
            reviewer_node_id: reviewer_node_id.into(),
            evidence_task_id: evidence_task_id.into(),
            evidence_scope_digest: evidence_scope_digest.into(),
            outcome,
            target_work_unit_keys: vec![
                "task|2|implementer|codex|none".into(),
                "task|1|implementer|codex|none".into(),
                "task|2|implementer|codex|none".into(),
            ],
            remediation_route_ids: vec!["route-b".into(), "route-a".into(), "route-b".into()],
        }
    }

    fn package_input(terminal_bytes: &[u8]) -> FinalFindingsPackageInputV1 {
        FinalFindingsPackageInputV1 {
            workflow_id: "workflow-1".into(),
            gate_id: "final".into(),
            gate_lineage: LINEAGE.into(),
            graph_revision: 9,
            findings: vec![
                finding("grok", "task-grok", SCOPE_B, CompletionOutcome::Block),
                finding(
                    "codex",
                    "task-codex",
                    SCOPE_A,
                    CompletionOutcome::RequestChanges,
                ),
            ],
            remediation_contexts: vec![
                RemediationContextInputV1::available_report(
                    "task-codex",
                    "reports/final-codex.md",
                    b"codex report".to_vec(),
                ),
                RemediationContextInputV1::available_terminal("task-grok", terminal_bytes.to_vec()),
            ],
        }
    }

    #[test]
    fn final_package_hashes_ordered_immutable_context_bytes() {
        let mut report_bytes = b"codex report".to_vec();
        let package = build_final_findings_package_v1(package_input(b"grok terminal")).unwrap();

        assert!(package.items[0].finding_id < package.items[1].finding_id);
        assert_eq!(
            package.items[0].target_work_unit_keys,
            vec![
                "task|1|implementer|codex|none",
                "task|2|implementer|codex|none"
            ]
        );
        assert_eq!(package.context_bytes(0).unwrap(), b"codex report");
        report_bytes.fill(b'x');
        assert_eq!(package.context_bytes(0).unwrap(), b"codex report");

        let changed = build_final_findings_package_v1(package_input(b"changed terminal")).unwrap();
        assert_ne!(package.package_digest, changed.package_digest);
        assert_ne!(
            package.final_findings_identity(),
            changed.final_findings_identity()
        );
    }

    #[test]
    fn final_source_evaluation_identity_binds_durable_routes() {
        let original = build_final_findings_package_v1(package_input(b"terminal")).unwrap();
        let mut changed_input = package_input(b"terminal");
        changed_input.findings[0]
            .remediation_route_ids
            .push("route-c".into());
        let changed = build_final_findings_package_v1(changed_input).unwrap();

        assert_ne!(
            original.source_evaluation_key,
            changed.source_evaluation_key
        );
    }

    #[test]
    fn final_nonpass_without_material_context_requires_decision() {
        let mut input = package_input(b"grok terminal");
        input.remediation_contexts = vec![RemediationContextInputV1::missing_report(
            "task-codex",
            "reports/missing.md",
        )];

        assert_eq!(
            build_final_findings_package_v1(input).unwrap_err(),
            FinalFindingsError::RemediationContextRequired
        );
    }

    #[test]
    fn final_package_rejects_corrupt_stored_context() {
        let mut package = build_final_findings_package_v1(package_input(b"grok terminal")).unwrap();
        package.remediation_contexts[0].content_base64 = Some("dGFtcGVyZWQ=".into());

        assert_eq!(
            verify_final_findings_package_v1(&package).unwrap_err(),
            FinalFindingsError::EvidenceCorrupt
        );
    }

    #[test]
    fn final_package_storage_round_trip_keeps_snapshot_authority() {
        let package = build_final_findings_package_v1(package_input(b"grok terminal")).unwrap();
        let record = encode_final_findings_package_v1(&package).unwrap();
        let decoded = decode_final_findings_package_v1(
            &package.workflow_id,
            &package.gate_id,
            &package.gate_lineage,
            &package.source_evaluation_key,
            &record.items_json,
            &record.remediation_contexts_json,
            &package.package_digest,
        )
        .unwrap();

        assert_eq!(decoded, package);
        let tampered = record
            .remediation_contexts_json
            .replace("Y29kZXggcmVwb3J0", "dGFtcGVyZWQ=");
        assert_eq!(
            decode_final_findings_package_v1(
                &package.workflow_id,
                &package.gate_id,
                &package.gate_lineage,
                &package.source_evaluation_key,
                &record.items_json,
                &tampered,
                &package.package_digest,
            )
            .unwrap_err(),
            FinalFindingsError::EvidenceCorrupt
        );
    }

    #[tokio::test]
    async fn report_context_capture_is_bounded_and_workspace_contained() {
        let workspace = tempfile::tempdir().unwrap();
        let report_path = workspace.path().join("reports/final.md");
        std::fs::create_dir_all(report_path.parent().unwrap()).unwrap();
        std::fs::write(&report_path, b"immutable report bytes").unwrap();

        let captured =
            capture_report_context_v1(workspace.path(), "review-task", "reports/final.md")
                .await
                .unwrap();
        assert_eq!(
            captured.bytes.as_deref(),
            Some(b"immutable report bytes".as_slice())
        );
        assert!(
            capture_report_context_v1(workspace.path(), "review-task", "../outside.md",)
                .await
                .is_err()
        );
    }
}
