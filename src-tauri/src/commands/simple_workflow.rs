use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, TransactionTrait,
};
use serde::{Deserialize, Serialize};

use crate::acp::delegation::route::DelegationRoutePolicy;
use crate::acp::delegation::workflow::key::normalize_rel_path;
use crate::acp::delegation::workflow::simple::{
    default_simple_progress_rel_path, register_simple_workflow_txn,
};
use crate::acp::delegation::workflow::types::ManifestDocument;
use crate::acp::delegation::workflow::{
    emit_workflow_compatibility_nudge, read_simple_plan, require_v2_mutation,
    resolve_conversation_workflow_mode, ConversationWorkflowMode, SimpleWorkflowError,
    WorkflowStoreError,
};
use crate::app_error::{AppCommandError, AppErrorCode};
use crate::db::entities::{
    conversation, delegation_workflow, delegation_workflow_manifest_revision, folder,
    simple_workflow,
};
use crate::db::service::conversation_service;
use crate::db::AppDatabase;
use crate::models::AgentType;
use crate::web::event_bridge::EventEmitter;

const MAX_CLIENT_REQUEST_TOKEN_BYTES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpleSuccessorResult {
    pub successor_conversation_id: i32,
    pub created: bool,
    pub plan_rel_path: String,
    pub progress_rel_path: String,
    pub bootstrap_prompt: String,
}

#[derive(Debug)]
struct ArchivedSource {
    root_conversation_id: i32,
    workflow_id: String,
    folder_id: i32,
    agent_type: AgentType,
    route_override: Option<DelegationRoutePolicy>,
    successor_title: Option<String>,
    plan_rel_path: String,
    design_rel_path: Option<String>,
}

fn validate_request_token(token: &str) -> Result<(), AppCommandError> {
    if token.is_empty()
        || token.len() > MAX_CLIENT_REQUEST_TOKEN_BYTES
        || token.chars().any(char::is_control)
    {
        return Err(AppCommandError::invalid_input(
            "client_request_token must be 1-256 bytes and contain no control characters",
        ));
    }
    Ok(())
}

fn workflow_error(error: WorkflowStoreError) -> AppCommandError {
    let message = error.to_string();
    let stable_code = error.code();
    match error {
        WorkflowStoreError::LegacyCompletionProtocolReadOnly => AppCommandError::new(
            AppErrorCode::LegacyCompletionProtocolReadOnly,
            message,
        )
        .with_detail(stable_code),
        WorkflowStoreError::UnsupportedCompletionProtocol { .. }
        | WorkflowStoreError::UnsupportedCompletionProtocolHeader(_) => AppCommandError::new(
            AppErrorCode::UnsupportedCompletionProtocol,
            message,
        )
        .with_detail(stable_code),
        WorkflowStoreError::WorkflowIdentityCorrupt { .. } => AppCommandError::new(
            AppErrorCode::WorkflowIdentityCorrupt,
            message,
        )
        .with_detail(stable_code),
        WorkflowStoreError::NotFound(_) | WorkflowStoreError::ParentNotFound(_) => {
            AppCommandError::not_found(message).with_detail(stable_code)
        }
        WorkflowStoreError::Persistence(_) => {
            AppCommandError::database_error(message).with_detail(stable_code)
        }
        _ => AppCommandError::invalid_input(message).with_detail(stable_code),
    }
}

fn simple_error(error: SimpleWorkflowError) -> AppCommandError {
    let message = error.to_string();
    let stable_code = error.code();
    match error {
        SimpleWorkflowError::Persistence(_) => {
            AppCommandError::database_error(message).with_detail(stable_code)
        }
        SimpleWorkflowError::ConversationNotFound(_)
        | SimpleWorkflowError::SourceWorkflowNotFound(_) => {
            AppCommandError::not_found(message).with_detail(stable_code)
        }
        SimpleWorkflowError::ModeConflict { .. }
        | SimpleWorkflowError::IdentityCorrupt { .. }
        | SimpleWorkflowError::SourceWorkflowMismatch => AppCommandError::new(
            AppErrorCode::WorkflowIdentityCorrupt,
            message,
        )
        .with_detail(stable_code),
        SimpleWorkflowError::Validation(_) => {
            AppCommandError::invalid_input(message).with_detail(stable_code)
        }
    }
}

fn plan_unavailable(plan_rel_path: &str) -> AppCommandError {
    let error = AppCommandError::new(
        AppErrorCode::SimpleSuccessorPlanUnavailable,
        "Archived Plan is unavailable",
    );
    match normalize_rel_path(plan_rel_path) {
        Ok(safe_rel_path) => error.with_detail(safe_rel_path),
        Err(_) => error,
    }
}

fn parse_route_override(
    raw: Option<&str>,
    source_conversation_id: i32,
) -> Result<Option<DelegationRoutePolicy>, AppCommandError> {
    match raw {
        None => Ok(None),
        Some("codeg") => Ok(Some(DelegationRoutePolicy::Codeg)),
        Some("native") => Ok(Some(DelegationRoutePolicy::Native)),
        Some(_) => Err(AppCommandError::new(
            AppErrorCode::WorkflowIdentityCorrupt,
            format!("conversation {source_conversation_id} has an invalid route override"),
        )
        .with_detail("workflow_identity_corrupt")),
    }
}

fn bootstrap_prompt(source: &ArchivedSource, progress_rel_path: &str) -> String {
    let mut lines = vec![
        "This is a Simple successor conversation.".to_string(),
        format!(
            "Archived source conversation: {}.",
            source.root_conversation_id
        ),
    ];
    if let Some(design_rel_path) = source.design_rel_path.as_deref() {
        lines.push(format!("Design: `{design_rel_path}`."));
    }
    lines.extend([
        format!("Plan: `{}`.", source.plan_rel_path),
        format!("Progress: `{progress_rel_path}`."),
        "Inspect Git and the filesystem before reconstructing repository-grounded progress."
            .to_string(),
        "Do not import archived workflow semantics or treat archived execution state as authority."
            .to_string(),
    ]);
    lines.join("\n")
}

async fn load_archived_source(
    db: &AppDatabase,
    source_conversation_id: i32,
) -> Result<ArchivedSource, AppCommandError> {
    let mode = resolve_conversation_workflow_mode(&db.conn, source_conversation_id)
        .await
        .map_err(simple_error)?;
    let (root_conversation_id, workflow_id) = match mode {
        ConversationWorkflowMode::Archived {
            root_conversation_id,
            workflow_id,
        } => (root_conversation_id, workflow_id),
        ConversationWorkflowMode::Corrupt {
            root_conversation_id,
            ..
        } => {
            return Err(AppCommandError::new(
                AppErrorCode::WorkflowIdentityCorrupt,
                format!(
                    "conversation {root_conversation_id} has conflicting workflow identities"
                ),
            )
            .with_detail("workflow_identity_corrupt"));
        }
        ConversationWorkflowMode::Ordinary { .. }
        | ConversationWorkflowMode::SimpleRegistered { .. }
        | ConversationWorkflowMode::SimpleObserved { .. } => {
            return Err(AppCommandError::invalid_input(
                "source conversation is not an archived workflow",
            ));
        }
    };

    let workflow = delegation_workflow::Entity::find_by_id(workflow_id.clone())
        .one(&db.conn)
        .await
        .map_err(|error| AppCommandError::database_error(error.to_string()))?
        .ok_or_else(|| AppCommandError::not_found("archived workflow was not found"))?;
    match require_v2_mutation(
        workflow.completion_protocol_version,
        &workflow.completion_protocol_mode,
    ) {
        Err(WorkflowStoreError::WorkflowV2Retired { .. }) => {}
        Err(error) => return Err(workflow_error(error)),
        Ok(()) => {
            return Err(AppCommandError::new(
                AppErrorCode::WorkflowIdentityCorrupt,
                "archived workflow unexpectedly remained writable",
            )
            .with_detail("workflow_identity_corrupt"));
        }
    }

    let source = conversation::Entity::find_by_id(root_conversation_id)
        .filter(conversation::Column::DeletedAt.is_null())
        .one(&db.conn)
        .await
        .map_err(|error| AppCommandError::database_error(error.to_string()))?
        .ok_or_else(|| AppCommandError::not_found("source conversation was not found"))?;
    if source.parent_id.is_some() {
        return Err(AppCommandError::new(
            AppErrorCode::WorkflowIdentityCorrupt,
            "archived workflow owner is not a root conversation",
        )
        .with_detail("workflow_identity_corrupt"));
    }
    let workspace = folder::Entity::find_by_id(source.folder_id)
        .filter(folder::Column::DeletedAt.is_null())
        .one(&db.conn)
        .await
        .map_err(|error| AppCommandError::database_error(error.to_string()))?
        .ok_or_else(|| plan_unavailable("workspace"))?;
    let revision = delegation_workflow_manifest_revision::Entity::find_by_id((
        workflow_id.clone(),
        workflow.active_manifest_revision,
    ))
    .one(&db.conn)
    .await
    .map_err(|error| AppCommandError::database_error(error.to_string()))?
    .ok_or_else(|| {
        AppCommandError::new(
            AppErrorCode::WorkflowIdentityCorrupt,
            "archived workflow active revision is missing",
        )
        .with_detail("workflow_identity_corrupt")
    })?;
    let document: ManifestDocument = serde_json::from_str(&revision.document_json).map_err(|_| {
        AppCommandError::new(
            AppErrorCode::WorkflowIdentityCorrupt,
            "archived workflow active revision is invalid",
        )
        .with_detail("workflow_identity_corrupt")
    })?;
    let raw_plan_rel_path = document.plan_target_rel_path;
    let plan_rel_path =
        normalize_rel_path(&raw_plan_rel_path).map_err(|_| plan_unavailable(&raw_plan_rel_path))?;
    read_simple_plan(std::path::Path::new(&workspace.path), &plan_rel_path)
        .await
        .map_err(|_| plan_unavailable(&plan_rel_path))?;

    let agent_type = AgentType::from_wire(&source.agent_type).ok_or_else(|| {
        AppCommandError::new(
            AppErrorCode::WorkflowIdentityCorrupt,
            format!(
                "conversation {root_conversation_id} has an invalid agent type"
            ),
        )
        .with_detail("workflow_identity_corrupt")
    })?;
    let route_override =
        parse_route_override(source.delegation_route_override.as_deref(), root_conversation_id)?;
    let successor_title = source.title.and_then(|title| {
        let title = title.trim();
        (!title.is_empty()).then(|| format!("{title} (Simple)"))
    });
    let design_rel_path = document
        .design
        .and_then(|design| normalize_rel_path(&design.rel_path).ok());

    Ok(ArchivedSource {
        root_conversation_id,
        workflow_id,
        folder_id: source.folder_id,
        agent_type,
        route_override,
        successor_title,
        plan_rel_path,
        design_rel_path,
    })
}

async fn load_existing_successor<C: ConnectionTrait>(
    conn: &C,
    source: &ArchivedSource,
) -> Result<Option<SimpleSuccessorResult>, AppCommandError> {
    let descriptor = simple_workflow::Entity::find()
        .filter(simple_workflow::Column::SourceWorkflowId.eq(source.workflow_id.clone()))
        .one(conn)
        .await
        .map_err(|error| AppCommandError::database_error(error.to_string()))?;
    let Some(descriptor) = descriptor else {
        return Ok(None);
    };
    if descriptor.plan_rel_path != source.plan_rel_path {
        return Err(AppCommandError::new(
            AppErrorCode::WorkflowIdentityCorrupt,
            "Simple successor locator conflicts with its archived source",
        )
        .with_detail("workflow_identity_corrupt"));
    }
    let successor = conversation::Entity::find_by_id(descriptor.parent_conversation_id)
        .filter(conversation::Column::DeletedAt.is_null())
        .one(conn)
        .await
        .map_err(|error| AppCommandError::database_error(error.to_string()))?;
    let Some(successor) = successor else {
        return Err(AppCommandError::new(
            AppErrorCode::WorkflowIdentityCorrupt,
            "Simple successor descriptor points to a deleted conversation",
        )
        .with_detail("workflow_identity_corrupt"));
    };
    if successor.parent_id.is_some() {
        return Err(AppCommandError::new(
            AppErrorCode::WorkflowIdentityCorrupt,
            "Simple successor is not a root conversation",
        )
        .with_detail("workflow_identity_corrupt"));
    }
    Ok(Some(SimpleSuccessorResult {
        successor_conversation_id: successor.id,
        created: false,
        plan_rel_path: descriptor.plan_rel_path,
        bootstrap_prompt: bootstrap_prompt(source, &descriptor.progress_rel_path),
        progress_rel_path: descriptor.progress_rel_path,
    }))
}

async fn create_or_load_successor(
    db: &AppDatabase,
    source: &ArchivedSource,
) -> Result<SimpleSuccessorResult, AppCommandError> {
    const MAX_RACE_ATTEMPTS: usize = 3;

    for attempt in 0..MAX_RACE_ATTEMPTS {
        if let Some(existing) = load_existing_successor(&db.conn, source).await? {
            return Ok(existing);
        }

        let txn = db
            .conn
            .begin()
            .await
            .map_err(|error| AppCommandError::database_error(error.to_string()))?;
        if let Some(existing) = load_existing_successor(&txn, source).await? {
            txn.commit()
                .await
                .map_err(|error| AppCommandError::database_error(error.to_string()))?;
            return Ok(existing);
        }

        let candidate = match conversation_service::create_root_with_route_override_in_transaction(
            &txn,
            source.folder_id,
            source.agent_type,
            source.successor_title.clone(),
            source.route_override,
        )
        .await
        {
            Ok(candidate) => candidate,
            Err(error) => {
                let _ = txn.rollback().await;
                if let Some(existing) = load_existing_successor(&db.conn, source).await? {
                    return Ok(existing);
                }
                return Err(AppCommandError::from(error));
            }
        };
        let progress_rel_path = default_simple_progress_rel_path(candidate.id);
        let registration = register_simple_workflow_txn(
            &txn,
            candidate.id,
            &source.plan_rel_path,
            Some(&progress_rel_path),
            Some(&source.workflow_id),
        )
        .await;
        if let Err(error) = registration {
            let _ = txn.rollback().await;
            if let Some(existing) = load_existing_successor(&db.conn, source).await? {
                return Ok(existing);
            }
            if attempt + 1 < MAX_RACE_ATTEMPTS {
                tokio::task::yield_now().await;
                continue;
            }
            return Err(simple_error(error));
        }

        match txn.commit().await {
            Ok(()) => {
                return Ok(SimpleSuccessorResult {
                    successor_conversation_id: candidate.id,
                    created: true,
                    plan_rel_path: source.plan_rel_path.clone(),
                    bootstrap_prompt: bootstrap_prompt(source, &progress_rel_path),
                    progress_rel_path,
                });
            }
            Err(error) => {
                if let Some(mut existing) = load_existing_successor(&db.conn, source).await? {
                    existing.created = existing.successor_conversation_id == candidate.id;
                    return Ok(existing);
                }
                if attempt + 1 < MAX_RACE_ATTEMPTS {
                    tokio::task::yield_now().await;
                    continue;
                }
                return Err(AppCommandError::database_error(error.to_string()));
            }
        }
    }
    Err(AppCommandError::database_error(
        "Simple successor creation did not converge",
    ))
}

pub async fn continue_archived_workflow_in_simple_core(
    db: &AppDatabase,
    emitter: &EventEmitter,
    source_conversation_id: i32,
    client_request_token: &str,
) -> Result<SimpleSuccessorResult, AppCommandError> {
    validate_request_token(client_request_token)?;
    let source = load_archived_source(db, source_conversation_id).await?;
    let result = create_or_load_successor(db, &source).await?;
    if result.created {
        crate::commands::conversations::emit_conversation_upsert(
            emitter,
            &db.conn,
            result.successor_conversation_id,
        )
        .await;
        emit_workflow_compatibility_nudge(emitter, source.root_conversation_id);
    }
    Ok(result)
}

#[cfg(feature = "tauri-runtime")]
#[tauri::command]
pub async fn continue_archived_workflow_in_simple(
    app: tauri::AppHandle,
    db: tauri::State<'_, AppDatabase>,
    source_conversation_id: i32,
    client_request_token: String,
) -> Result<SimpleSuccessorResult, AppCommandError> {
    continue_archived_workflow_in_simple_core(
        &db,
        &EventEmitter::Tauri(app),
        source_conversation_id,
        &client_request_token,
    )
    .await
}

#[cfg(test)]
pub(crate) mod test_support {
    use chrono::Utc;
    use sea_orm::{ActiveModelTrait, Set};

    use crate::acp::delegation::workflow::key::build_work_unit_key;
    use crate::acp::delegation::workflow::types::{
        DocumentGateKind, DocumentRef, ManifestDocument, ManifestGate, ManifestNode,
        ManifestNodeKind, ManifestNodeRole, ManifestPhase, ManifestWorkflowState, ResolutionMode,
        WorkUnitKeyParts, MANIFEST_SCHEMA_VERSION, PHASE_DESIGN, PHASE_PLAN,
        TASK_RISK_POLICY_VERSION, WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY,
    };
    use crate::db::entities::delegation_task_run::{
        self, AdmissionClass, DelegationRunStatus,
    };
    use crate::db::entities::delegation_workflow::{
        self, CompletionProtocolMode, WorkflowState,
    };
    use crate::db::entities::{
        delegation_workflow_manifest_revision, delegation_workflow_run_binding,
    };
    use crate::db::AppDatabase;

    fn phase(id: &str) -> ManifestPhase {
        ManifestPhase {
            id: id.into(),
            kind: Some(id.into()),
            title: None,
        }
    }

    pub fn archived_manifest(
        token: &str,
        plan_rel_path: &str,
        design_rel_path: Option<&str>,
    ) -> ManifestDocument {
        let plan_author_key = build_work_unit_key(&WorkUnitKeyParts::PlanAuthor {
            rel_plan_path: plan_rel_path,
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap_or_else(|_| {
            build_work_unit_key(&WorkUnitKeyParts::PlanAuthor {
                rel_plan_path: "docs/plan.md",
                agent_type: "codex",
                profile_id: None,
            })
            .expect("fallback Plan author key")
        });
        let mut phases = vec![phase(PHASE_PLAN)];
        let mut nodes = vec![ManifestNode {
            id: "plan-author".into(),
            kind: ManifestNodeKind::WorkUnit,
            phase_id: Some(PHASE_PLAN.into()),
            role: Some(ManifestNodeRole::Author),
            agent_type: Some("codex".into()),
            profile_id: None,
            task_index: None,
            work_unit_key: Some(plan_author_key),
            deps: vec![],
            required: Some(true),
            node_outcome: None,
            title: None,
        }];
        let mut gates = Vec::new();
        let design = design_rel_path.map(|rel_path| {
            phases.insert(0, phase(PHASE_DESIGN));
            let key = build_work_unit_key(&WorkUnitKeyParts::Design {
                rel_doc_path: rel_path,
                agent_type: "codex",
                profile_id: None,
            })
            .expect("Design reviewer key");
            nodes.insert(
                0,
                ManifestNode {
                    id: "design-reviewer".into(),
                    kind: ManifestNodeKind::WorkUnit,
                    phase_id: Some(PHASE_DESIGN.into()),
                    role: Some(ManifestNodeRole::Reviewer),
                    agent_type: Some("codex".into()),
                    profile_id: None,
                    task_index: None,
                    work_unit_key: Some(key),
                    deps: vec![],
                    required: Some(true),
                    node_outcome: None,
                    title: None,
                },
            );
            gates.push(ManifestGate {
                id: "design".into(),
                reviewer_cohort_node_ids: vec!["design-reviewer".into()],
                required_reviewer_node_ids: vec!["design-reviewer".into()],
                resolution_mode: ResolutionMode::ParentAdjudication,
                gate_kind: Some(DocumentGateKind::Design),
            });
            DocumentRef {
                rel_path: rel_path.into(),
                digest: format!("sha256:{}", "d".repeat(64)),
            }
        });

        ManifestDocument {
            schema_version: MANIFEST_SCHEMA_VERSION,
            workflow_kind: WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY.into(),
            plan_target_rel_path: plan_rel_path.into(),
            risk_policy_version: TASK_RISK_POLICY_VERSION.into(),
            workflow_id: None,
            expected_manifest_revision: None,
            publication_token: token.into(),
            workflow_state: ManifestWorkflowState::Skeleton,
            design,
            plan: None,
            phases,
            nodes,
            edges: vec![],
            gates,
            task_policies: vec![],
        }
    }

    pub async fn seed_archived_workflow(
        db: &AppDatabase,
        parent_conversation_id: i32,
        workflow_id: &str,
        plan_rel_path: &str,
        design_rel_path: Option<&str>,
        version: i64,
        mode: CompletionProtocolMode,
    ) {
        let now = Utc::now();
        let document = archived_manifest(
            &format!("publication-{workflow_id}"),
            plan_rel_path,
            design_rel_path,
        );
        delegation_workflow::ActiveModel {
            workflow_id: Set(workflow_id.into()),
            parent_conversation_id: Set(parent_conversation_id),
            workflow_kind: Set(WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY.into()),
            schema_version: Set(MANIFEST_SCHEMA_VERSION as i64),
            active_manifest_revision: Set(1),
            graph_revision: Set(7),
            workflow_state: Set(WorkflowState::Skeleton),
            capability_version: Set("workflow_manifest_v2".into()),
            publication_token: Set(format!("publication-{workflow_id}")),
            supersedes_approved_revision: Set(None),
            structural_revision: Set(1),
            design_fingerprint: Set("design-fingerprint".into()),
            plan_fingerprint: Set("plan-fingerprint".into()),
            block_cause_code: Set(None),
            block_source_manifest_revision: Set(None),
            completion_protocol_version: Set(version),
            completion_protocol_mode: Set(mode),
            legacy_source_workflow_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&db.conn)
        .await
        .expect("archived workflow header");
        delegation_workflow_manifest_revision::ActiveModel {
            workflow_id: Set(workflow_id.into()),
            manifest_revision: Set(1),
            manifest_state: Set("skeleton".into()),
            document_json: Set(serde_json::to_string(&document).expect("manifest JSON")),
            document_digest: Set(format!("sha256:{}", "a".repeat(64))),
            created_at: Set(now),
            ..Default::default()
        }
        .insert(&db.conn)
        .await
        .expect("archived workflow revision");
    }

    pub async fn seed_bound_child(
        db: &AppDatabase,
        root_conversation_id: i32,
        child_conversation_id: i32,
        workflow_id: &str,
    ) {
        let now = Utc::now();
        delegation_task_run::ActiveModel {
            task_id: Set(format!("{workflow_id}-bound-task")),
            root_task_id: Set(format!("{workflow_id}-bound-task")),
            previous_task_id: Set(None),
            generation: Set(1),
            parent_conversation_id: Set(root_conversation_id),
            parent_tool_use_id: Set(None),
            child_conversation_id: Set(child_conversation_id),
            agent_type: Set("codex".into()),
            admission_class: Set(AdmissionClass::NormalRevision),
            lineage_root_task_id: Set(format!("{workflow_id}-bound-task")),
            history_only: Set(false),
            status: Set(DelegationRunStatus::Completed),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(&db.conn)
        .await
        .expect("bound child run");
        delegation_workflow_run_binding::ActiveModel {
            task_id: Set(format!("{workflow_id}-bound-task")),
            workflow_id: Set(workflow_id.into()),
            node_id: Set("archived-node".into()),
            manifest_revision: Set(1),
            lineage_ordinal: Set(1),
            summary_validated: Set(false),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(&db.conn)
        .await
        .expect("bound child workflow binding");
    }
}

#[cfg(test)]
mod tests {
    use sea_orm::{
        ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, Set,
    };

    use super::test_support::{seed_archived_workflow, seed_bound_child};
    use super::*;
    use crate::acp::delegation::route::DelegationRoutePolicy;
    use crate::acp::delegation::workflow::plan_material::MAX_PLAN_MATERIAL_BYTES;
    use crate::acp::delegation::workflow::{
        load_simple_workflow, register_simple_workflow,
        workflow_v2_retired_for_conversation,
    };
    use crate::app_error::AppErrorCode;
    use crate::app_state::AppState;
    use crate::commands::conversations::delete_conversation_with_cleanup_core;
    use crate::db::entities::delegation_workflow::CompletionProtocolMode;
    use crate::db::entities::{
        conversation, delegation_task_run, delegation_workflow,
        delegation_workflow_manifest_revision, delegation_workflow_run_binding, simple_workflow,
    };
    use crate::db::test_helpers::{
        fresh_disk_db, fresh_in_memory_db, seed_conversation, seed_folder,
    };
    use crate::models::AgentType;
    use crate::web::event_bridge::EventEmitter;

    async fn live_conversation_count(db: &crate::db::AppDatabase) -> u64 {
        conversation::Entity::find()
            .filter(conversation::Column::DeletedAt.is_null())
            .count(&db.conn)
            .await
            .expect("conversation count")
    }

    async fn descriptor_count(db: &crate::db::AppDatabase) -> u64 {
        simple_workflow::Entity::find()
            .count(&db.conn)
            .await
            .expect("descriptor count")
    }

    #[tokio::test]
    async fn simple_successor_root_inherits_only_safe_root_identity_and_preserves_v2_rows() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::create_dir_all(workspace.path().join("docs/plans")).unwrap();
        std::fs::create_dir_all(workspace.path().join("docs/specs")).unwrap();
        std::fs::write(workspace.path().join("docs/plans/ship.md"), "# Plan\n").unwrap();
        std::fs::write(workspace.path().join("docs/specs/design.md"), "# Design\n").unwrap();

        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, workspace.path().to_str().unwrap()).await;
        let source = seed_conversation(&db, folder, AgentType::Codex).await;
        let source_row = conversation::Entity::find_by_id(source)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut source_active: conversation::ActiveModel = source_row.into();
        source_active.title = Set(Some("Archived delivery".into()));
        source_active.delegation_route_override = Set(Some("codeg".into()));
        source_active.update(&db.conn).await.unwrap();
        seed_archived_workflow(
            &db,
            source,
            "workflow-successor-root",
            "docs/plans/ship.md",
            Some("docs/specs/design.md"),
            2,
            CompletionProtocolMode::V2Enforce,
        )
        .await;

        let header_before = delegation_workflow::Entity::find_by_id("workflow-successor-root")
            .one(&db.conn)
            .await
            .unwrap();
        let revisions_before = delegation_workflow_manifest_revision::Entity::find()
            .count(&db.conn)
            .await
            .unwrap();
        let runs_before = delegation_task_run::Entity::find()
            .count(&db.conn)
            .await
            .unwrap();
        let bindings_before = delegation_workflow_run_binding::Entity::find()
            .count(&db.conn)
            .await
            .unwrap();

        let result = continue_archived_workflow_in_simple_core(
            &db,
            &EventEmitter::Noop,
            source,
            "successor-request-root",
        )
        .await
        .expect("create successor");

        assert!(result.created);
        assert_eq!(result.plan_rel_path, "docs/plans/ship.md");
        assert_eq!(
            result.progress_rel_path,
            format!(
                ".superpowers/sdd/{}/progress.md",
                result.successor_conversation_id
            )
        );
        assert!(result.bootstrap_prompt.contains("docs/plans/ship.md"));
        assert!(result.bootstrap_prompt.contains("docs/specs/design.md"));
        assert!(result.bootstrap_prompt.contains(&result.progress_rel_path));
        assert!(result.bootstrap_prompt.contains(&source.to_string()));
        assert!(!result.bootstrap_prompt.contains("workflow-successor-root"));
        for forbidden in [
            "gate ID",
            "task ID",
            "approval outcome",
            "completion Card",
            "evidence counter",
            "recovery counter",
        ] {
            assert!(!result.bootstrap_prompt.contains(forbidden));
        }

        let successor = conversation::Entity::find_by_id(result.successor_conversation_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(successor.folder_id, folder);
        assert_eq!(successor.parent_id, None);
        assert_eq!(successor.agent_type, "codex");
        assert_eq!(successor.title.as_deref(), Some("Archived delivery (Simple)"));
        assert_eq!(
            successor.delegation_route_override.as_deref(),
            Some("codeg")
        );
        assert_eq!(successor.git_branch, None);
        assert_eq!(successor.model, None);
        assert_eq!(successor.external_id, None);

        let descriptor = load_simple_workflow(&db.conn, result.successor_conversation_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            descriptor.source_workflow_id.as_deref(),
            Some("workflow-successor-root")
        );
        let retired_navigation =
            workflow_v2_retired_for_conversation(&db.conn, source)
                .await
                .unwrap();
        assert_eq!(
            retired_navigation.successor_conversation_id(),
            Some(result.successor_conversation_id)
        );
        assert_eq!(
            retired_navigation.can_create_simple_successor(),
            Some(false)
        );
        assert_eq!(
            header_before,
            delegation_workflow::Entity::find_by_id("workflow-successor-root")
                .one(&db.conn)
                .await
                .unwrap()
        );
        assert_eq!(
            revisions_before,
            delegation_workflow_manifest_revision::Entity::find()
                .count(&db.conn)
                .await
                .unwrap()
        );
        assert_eq!(
            runs_before,
            delegation_task_run::Entity::find()
                .count(&db.conn)
                .await
                .unwrap()
        );
        assert_eq!(
            bindings_before,
            delegation_workflow_run_binding::Entity::find()
                .count(&db.conn)
                .await
                .unwrap()
        );

        let replay = continue_archived_workflow_in_simple_core(
            &db,
            &EventEmitter::Noop,
            source,
            "successor-request-root",
        )
        .await
        .expect("reopen successor");
        assert!(!replay.created);
        assert_eq!(replay.successor_conversation_id, result.successor_conversation_id);
    }

    #[tokio::test]
    async fn simple_successor_bound_child_resolves_to_archived_root() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace.path().join("docs")).unwrap();
        std::fs::write(workspace.path().join("docs/plan.md"), "# Plan\n").unwrap();
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, workspace.path().to_str().unwrap()).await;
        let root = seed_conversation(&db, folder, AgentType::Grok).await;
        let child = seed_conversation(&db, folder, AgentType::Codex).await;
        seed_archived_workflow(
            &db,
            root,
            "workflow-successor-child",
            "docs/plan.md",
            None,
            2,
            CompletionProtocolMode::V2Enforce,
        )
        .await;
        seed_bound_child(&db, root, child, "workflow-successor-child").await;

        let result = continue_archived_workflow_in_simple_core(
            &db,
            &EventEmitter::Noop,
            child,
            "successor-request-child",
        )
        .await
        .expect("create from bound child");
        let successor = conversation::Entity::find_by_id(result.successor_conversation_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(successor.agent_type, "grok");
        assert!(result.bootstrap_prompt.contains(&root.to_string()));
        assert!(!result.bootstrap_prompt.contains(&format!("conversation {child}")));
        let retired_navigation =
            workflow_v2_retired_for_conversation(&db.conn, child)
                .await
                .unwrap();
        assert_eq!(retired_navigation.source_conversation_id(), Some(root));
        assert_eq!(
            retired_navigation.successor_conversation_id(),
            Some(result.successor_conversation_id)
        );
        assert_eq!(
            retired_navigation.can_create_simple_successor(),
            Some(false)
        );
    }

    #[tokio::test]
    async fn simple_successor_rejects_ordinary_simple_legacy_and_corrupt_sources() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace.path().join("docs")).unwrap();
        std::fs::write(workspace.path().join("docs/plan.md"), "# Plan\n").unwrap();
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, workspace.path().to_str().unwrap()).await;

        let ordinary = seed_conversation(&db, folder, AgentType::Codex).await;
        let ordinary_error = continue_archived_workflow_in_simple_core(
            &db,
            &EventEmitter::Noop,
            ordinary,
            "ordinary-request",
        )
        .await
        .unwrap_err();
        assert_eq!(ordinary_error.code, AppErrorCode::InvalidInput);

        let simple = seed_conversation(&db, folder, AgentType::Codex).await;
        register_simple_workflow(&db.conn, simple, "docs/plan.md", None)
            .await
            .unwrap();
        let simple_error = continue_archived_workflow_in_simple_core(
            &db,
            &EventEmitter::Noop,
            simple,
            "simple-request",
        )
        .await
        .unwrap_err();
        assert_eq!(simple_error.code, AppErrorCode::InvalidInput);

        let legacy_db =
            crate::db::test_helpers::historical_completion_protocol_db_before_v2_only().await;
        let legacy_folder = seed_folder(
            &legacy_db,
            workspace.path().to_str().unwrap(),
        )
        .await;
        let legacy = seed_conversation(&legacy_db, legacy_folder, AgentType::Codex).await;
        seed_archived_workflow(
            &legacy_db,
            legacy,
            "workflow-successor-legacy",
            "docs/plan.md",
            None,
            1,
            CompletionProtocolMode::V1,
        )
        .await;
        crate::db::test_helpers::complete_historical_completion_protocol_migrations(&legacy_db)
            .await;
        let legacy_error = continue_archived_workflow_in_simple_core(
            &legacy_db,
            &EventEmitter::Noop,
            legacy,
            "legacy-request",
        )
        .await
        .unwrap_err();
        assert_eq!(
            legacy_error.code,
            AppErrorCode::LegacyCompletionProtocolReadOnly
        );

        let corrupt = seed_conversation(&db, folder, AgentType::Codex).await;
        seed_archived_workflow(
            &db,
            corrupt,
            "workflow-successor-corrupt",
            "docs/plan.md",
            None,
            2,
            CompletionProtocolMode::V2Enforce,
        )
        .await;
        simple_workflow::ActiveModel {
            parent_conversation_id: Set(corrupt),
            plan_rel_path: Set("docs/plan.md".into()),
            progress_rel_path: Set("state/progress.md".into()),
            source_workflow_id: Set(None),
            created_at: Set(chrono::Utc::now()),
            updated_at: Set(chrono::Utc::now()),
        }
        .insert(&db.conn)
        .await
        .unwrap();
        let corrupt_error = continue_archived_workflow_in_simple_core(
            &db,
            &EventEmitter::Noop,
            corrupt,
            "corrupt-request",
        )
        .await
        .unwrap_err();
        assert_eq!(corrupt_error.code, AppErrorCode::WorkflowIdentityCorrupt);
    }

    #[tokio::test]
    async fn simple_successor_rejects_invalid_request_tokens_before_writes() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace.path().join("docs")).unwrap();
        std::fs::write(workspace.path().join("docs/plan.md"), "# Plan\n").unwrap();
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, workspace.path().to_str().unwrap()).await;
        let source = seed_conversation(&db, folder, AgentType::Codex).await;
        seed_archived_workflow(
            &db,
            source,
            "workflow-invalid-successor-token",
            "docs/plan.md",
            None,
            2,
            CompletionProtocolMode::V2Enforce,
        )
        .await;
        let conversations_before = live_conversation_count(&db).await;

        for token in [
            String::new(),
            "invalid\nrequest".to_string(),
            "x".repeat(MAX_CLIENT_REQUEST_TOKEN_BYTES + 1),
        ] {
            let error = continue_archived_workflow_in_simple_core(
                &db,
                &EventEmitter::Noop,
                source,
                &token,
            )
            .await
            .unwrap_err();
            assert_eq!(error.code, AppErrorCode::InvalidInput);
        }
        assert_eq!(live_conversation_count(&db).await, conversations_before);
        assert_eq!(descriptor_count(&db).await, 0);
    }

    #[tokio::test]
    async fn simple_successor_plan_failures_are_stable_and_write_nothing() {
        #[derive(Clone, Copy)]
        enum Failure {
            Missing,
            Escaped,
            Absolute,
            Oversized,
            NonUtf8,
        }

        for (index, failure) in [
            Failure::Missing,
            Failure::Escaped,
            Failure::Absolute,
            Failure::Oversized,
            Failure::NonUtf8,
        ]
        .into_iter()
        .enumerate()
        {
            let workspace = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(workspace.path().join("docs")).unwrap();
            let absolute_plan_path = workspace.path().join("outside.md");
            let plan_rel_path = match failure {
                Failure::Escaped => "../outside.md".to_string(),
                Failure::Absolute => absolute_plan_path.to_string_lossy().into_owned(),
                _ => "docs/plan.md".to_string(),
            };
            match failure {
                Failure::Missing | Failure::Escaped | Failure::Absolute => {}
                Failure::Oversized => std::fs::write(
                    workspace.path().join("docs/plan.md"),
                    vec![b'x'; MAX_PLAN_MATERIAL_BYTES + 1],
                )
                .unwrap(),
                Failure::NonUtf8 => {
                    std::fs::write(workspace.path().join("docs/plan.md"), [0xff]).unwrap()
                }
            }
            let db = fresh_in_memory_db().await;
            let folder = seed_folder(&db, workspace.path().to_str().unwrap()).await;
            let source = seed_conversation(&db, folder, AgentType::Codex).await;
            seed_archived_workflow(
                &db,
                source,
                &format!("workflow-plan-failure-{index}"),
                &plan_rel_path,
                None,
                2,
                CompletionProtocolMode::V2Enforce,
            )
            .await;
            let conversations_before = live_conversation_count(&db).await;
            let descriptors_before = descriptor_count(&db).await;

            let error = continue_archived_workflow_in_simple_core(
                &db,
                &EventEmitter::Noop,
                source,
                &format!("plan-failure-request-{index}"),
            )
            .await
            .unwrap_err();
            assert_eq!(error.code, AppErrorCode::SimpleSuccessorPlanUnavailable);
            assert_eq!(live_conversation_count(&db).await, conversations_before);
            assert_eq!(descriptor_count(&db).await, descriptors_before);
            assert!(!error.to_string().contains(workspace.path().to_str().unwrap()));
            if matches!(failure, Failure::Absolute) {
                assert_eq!(error.detail, None);
            }
        }
    }

    #[tokio::test]
    async fn simple_successor_concurrent_requests_converge_on_unique_source_link() {
        let workspace = tempfile::tempdir().unwrap();
        let database = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace.path().join("docs")).unwrap();
        std::fs::write(workspace.path().join("docs/plan.md"), "# Plan\n").unwrap();
        let db = fresh_disk_db(database.path()).await;
        let folder = seed_folder(&db, workspace.path().to_str().unwrap()).await;
        let source = seed_conversation(&db, folder, AgentType::Codex).await;
        seed_archived_workflow(
            &db,
            source,
            "workflow-successor-concurrent",
            "docs/plan.md",
            None,
            2,
            CompletionProtocolMode::V2Enforce,
        )
        .await;
        let conversations_before = live_conversation_count(&db).await;

        let (first, second) = tokio::join!(
            continue_archived_workflow_in_simple_core(
                &db,
                &EventEmitter::Noop,
                source,
                "concurrent-request-a"
            ),
            continue_archived_workflow_in_simple_core(
                &db,
                &EventEmitter::Noop,
                source,
                "concurrent-request-b"
            )
        );
        let first = first.unwrap();
        let second = second.unwrap();
        assert_eq!(first.successor_conversation_id, second.successor_conversation_id);
        assert_eq!(usize::from(first.created) + usize::from(second.created), 1);
        assert_eq!(live_conversation_count(&db).await, conversations_before + 1);
        assert_eq!(descriptor_count(&db).await, 1);
    }

    #[tokio::test]
    async fn simple_successor_public_deletion_releases_link_and_allows_recreation() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace.path().join("docs")).unwrap();
        std::fs::write(workspace.path().join("docs/plan.md"), "# Plan\n").unwrap();
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, workspace.path().to_str().unwrap()).await;
        let source = seed_conversation(&db, folder, AgentType::Codex).await;
        seed_archived_workflow(
            &db,
            source,
            "workflow-successor-recreate",
            "docs/plan.md",
            None,
            2,
            CompletionProtocolMode::V2Enforce,
        )
        .await;
        let state = AppState::new_for_test(db, workspace.path().to_path_buf());

        let first = continue_archived_workflow_in_simple_core(
            &state.db,
            &state.emitter,
            source,
            "recreate-request-a",
        )
        .await
        .unwrap();
        delete_conversation_with_cleanup_core(
            &state.emitter,
            &state.db.conn,
            state.auto_title_coordinator.as_ref(),
            first.successor_conversation_id,
        )
        .await
        .expect("public successor delete");
        assert!(load_simple_workflow(&state.db.conn, first.successor_conversation_id)
            .await
            .unwrap()
            .is_none());

        let second = continue_archived_workflow_in_simple_core(
            &state.db,
            &state.emitter,
            source,
            "recreate-request-b",
        )
        .await
        .unwrap();
        assert!(second.created);
        assert_ne!(second.successor_conversation_id, first.successor_conversation_id);
    }

    #[test]
    fn simple_successor_tauri_command_is_registered() {
        let lib_source = include_str!("../lib.rs");
        assert!(lib_source.contains(
            "crate::commands::simple_workflow::continue_archived_workflow_in_simple"
        ));
    }

    #[test]
    fn route_override_type_used_by_successor_stays_wire_stable() {
        assert_eq!(
            serde_json::to_value(DelegationRoutePolicy::Codeg).unwrap(),
            "codeg"
        );
    }
}
