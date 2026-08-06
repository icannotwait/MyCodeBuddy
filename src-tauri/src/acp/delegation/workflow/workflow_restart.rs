//! Immutable legacy-to-v2 restart. The legacy workflow is only read; a fresh
//! root conversation and minimal v2 skeleton are committed atomically.

use chrono::Utc;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Set, TransactionTrait,
};

use crate::db::entities::{
    auto_title_job, conversation, delegation_workflow, delegation_workflow_manifest_revision,
    delegation_workflow_node_binding, delegation_workflow_restart_context,
};
use crate::db::AppDatabase;

use super::error::WorkflowStoreError;
use super::key::build_work_unit_key;
use super::store::{
    design_fingerprint_hash, normalized_to_document, plan_fingerprint_hash, sha256_hex,
    WORKFLOW_CAPABILITY_VERSION,
};
use super::types::{
    CompletionProtocolSelection, CompletionProtocolWorkflowProjection, DocumentGateKind,
    LegacyWorkflowLink, LegacyWorkflowRestartContext, LegacyWorkflowRestartProjection,
    ManifestDocument, ManifestNode, ManifestNodeKind, ManifestNodeRole, ManifestPhase,
    ManifestWorkflowState, WorkUnitKeyParts, MANIFEST_SCHEMA_VERSION, PHASE_DESIGN, PHASE_PLAN,
    WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY,
};
use super::validate::validate_manifest_document;

const LEGACY_RESTART_REASON: &str = "legacy_completion_protocol_restart_required";

/// Capture the first accepted root request once. The visible projection is
/// capped to 4,000 scalars (at most 16 KiB UTF-8), and oversized untrusted
/// message identities collapse to a fixed digest.
pub async fn capture_original_request_context<C: ConnectionTrait>(
    conn: &C,
    conversation_id: i32,
    original_request_id: &str,
    blocks: &[crate::acp::types::PromptInputBlock],
    agent_type: &str,
) -> Result<(), sea_orm::DbErr> {
    let request_text =
        crate::auto_title::bound_context(&crate::auto_title::project_visible_prompt(blocks));
    if request_text.trim().is_empty() {
        return Ok(());
    }
    let original_request_id = if original_request_id.chars().count() <= 200 {
        original_request_id.to_string()
    } else {
        format!("sha256:{}", sha256_hex(original_request_id.as_bytes()))
    };
    let now = Utc::now();
    let model = delegation_workflow_restart_context::ActiveModel {
        conversation_id: Set(conversation_id),
        original_conversation_id: Set(conversation_id),
        original_request_id: Set(original_request_id),
        original_request_digest: Set(format!("sha256:{}", sha256_hex(request_text.as_bytes()))),
        original_request_text: Set(request_text),
        agent_type: Set(agent_type.to_string()),
        profile_id: Set(None),
        created_at: Set(now),
    };
    match delegation_workflow_restart_context::Entity::insert(model)
        .on_conflict(
            OnConflict::column(delegation_workflow_restart_context::Column::ConversationId)
                .do_nothing()
                .to_owned(),
        )
        .exec(conn)
        .await
    {
        Ok(_) | Err(sea_orm::DbErr::RecordNotInserted) => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(any(test, feature = "test-utils"))]
thread_local! {
    static FAIL_RESTART_HEADER_ONCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(any(test, feature = "test-utils"))]
pub fn inject_legacy_restart_header_failure_once() {
    FAIL_RESTART_HEADER_ONCE.set(true);
}

fn take_restart_header_failure() -> bool {
    #[cfg(any(test, feature = "test-utils"))]
    {
        return FAIL_RESTART_HEADER_ONCE.replace(false);
    }
    #[cfg(not(any(test, feature = "test-utils")))]
    false
}

struct RestartCommit {
    source_workflow_id: String,
    source_conversation_id: i32,
    successor_workflow_id: String,
    successor_conversation_id: i32,
    idempotent_replay: bool,
}

/// Create or replay the single v2 successor for a legacy root conversation.
/// Authentication is intentionally performed by the desktop/Web/MCP adapter
/// before entering this transaction.
pub async fn restart_legacy_workflow_core(
    db: &AppDatabase,
    source_conversation_id: i64,
) -> Result<LegacyWorkflowRestartProjection, WorkflowStoreError> {
    let source_conversation_id = i32::try_from(source_conversation_id).map_err(|_| {
        WorkflowStoreError::LegacyCompletionProtocolRestartInvalid(
            "source_conversation_id is outside the supported range".into(),
        )
    })?;
    let now = Utc::now();
    let committed = db
        .conn
        .transaction::<_, RestartCommit, WorkflowStoreError>(|txn| {
            Box::pin(async move {
                let source = delegation_workflow::Entity::find()
                    .filter(
                        delegation_workflow::Column::ParentConversationId
                            .eq(source_conversation_id),
                    )
                    .filter(
                        delegation_workflow::Column::WorkflowKind
                            .eq(WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY),
                    )
                    .one(txn)
                    .await
                    .map_err(db_err)?
                    .ok_or_else(|| WorkflowStoreError::ParentNotFound(source_conversation_id))?;
                if source.completion_protocol_version != 1 {
                    return Err(WorkflowStoreError::LegacyCompletionProtocolRestartInvalid(
                        "source workflow is not protocol v1".into(),
                    ));
                }

                if let Some(existing) = delegation_workflow::Entity::find()
                    .filter(
                        delegation_workflow::Column::LegacySourceWorkflowId
                            .eq(source.workflow_id.clone()),
                    )
                    .one(txn)
                    .await
                    .map_err(db_err)?
                {
                    return Ok(RestartCommit {
                        source_workflow_id: source.workflow_id,
                        source_conversation_id,
                        successor_workflow_id: existing.workflow_id,
                        successor_conversation_id: existing.parent_conversation_id,
                        idempotent_replay: true,
                    });
                }

                let source_conversation = conversation::Entity::find_by_id(source_conversation_id)
                    .one(txn)
                    .await
                    .map_err(db_err)?
                    .ok_or(WorkflowStoreError::ParentNotFound(source_conversation_id))?;
                if source_conversation.deleted_at.is_some()
                    || source_conversation.kind == conversation::ConversationKind::Delegate
                    || source_conversation.parent_id.is_some()
                {
                    return Err(WorkflowStoreError::LegacyCompletionProtocolRestartInvalid(
                        "source must be a live root conversation".into(),
                    ));
                }
                let source_author = delegation_workflow_node_binding::Entity::find()
                    .filter(
                        delegation_workflow_node_binding::Column::WorkflowId
                            .eq(source.workflow_id.clone()),
                    )
                    .filter(delegation_workflow_node_binding::Column::Role.eq("author"))
                    .one(txn)
                    .await
                    .map_err(db_err)?;
                let (author_agent_type, author_profile_id) = source_author
                    .map(|binding| (binding.agent_type, binding.profile_id))
                    .unwrap_or_else(|| (source_conversation.agent_type.clone(), None));
                let source_restart_context =
                    delegation_workflow_restart_context::Entity::find_by_id(source_conversation_id)
                        .one(txn)
                        .await
                        .map_err(db_err)?
                        .ok_or_else(|| {
                            WorkflowStoreError::LegacyCompletionProtocolRestartRequired(
                                "original request context is unavailable".into(),
                            )
                        })?;

                let successor_conversation = conversation::ActiveModel {
                    id: sea_orm::NotSet,
                    folder_id: Set(source_conversation.folder_id),
                    title: Set(source_conversation.title.clone()),
                    title_locked: Set(source_conversation.title_locked),
                    auto_title_finalized: Set(source_conversation.auto_title_finalized),
                    agent_type: Set(source_conversation.agent_type.clone()),
                    status: Set(conversation::ConversationStatus::InProgress),
                    kind: Set(source_conversation.kind.clone()),
                    model: Set(source_conversation.model.clone()),
                    git_branch: Set(source_conversation.git_branch.clone()),
                    external_id: Set(None),
                    parent_id: Set(None),
                    parent_tool_use_id: Set(None),
                    delegation_call_id: Set(None),
                    delegation_route_override: Set(source_conversation
                        .delegation_route_override
                        .clone()),
                    delegation_task_status: Set(None),
                    delegation_error_code: Set(None),
                    delegation_started_at: Set(None),
                    delegation_finished_at: Set(None),
                    delegation_tool_call_count: Set(None),
                    delegation_edit_tool_call_count: Set(None),
                    delegation_touched_files_json: Set(None),
                    delegation_touched_files_truncated: Set(None),
                    delegation_additions: Set(None),
                    delegation_deletions: Set(None),
                    delegation_line_counts_complete: Set(None),
                    message_count: Set(0),
                    created_at: Set(now),
                    updated_at: Set(now),
                    deleted_at: Set(None),
                    pinned_at: Set(None),
                    awaiting_reply_token: Set(None),
                    delegation_run_generation: Set(None),
                    last_termination_audit_json: Set(None),
                }
                .insert(txn)
                .await
                .map_err(db_err)?;

                delegation_workflow_restart_context::ActiveModel {
                    conversation_id: Set(successor_conversation.id),
                    original_conversation_id: Set(source_restart_context.original_conversation_id),
                    original_request_id: Set(source_restart_context.original_request_id),
                    original_request_text: Set(source_restart_context.original_request_text),
                    original_request_digest: Set(source_restart_context.original_request_digest),
                    agent_type: Set(author_agent_type.clone()),
                    profile_id: Set(author_profile_id.clone()),
                    created_at: Set(now),
                }
                .insert(txn)
                .await
                .map_err(db_err)?;

                if let Some(source_job) = auto_title_job::Entity::find_by_id(source_conversation_id)
                    .one(txn)
                    .await
                    .map_err(db_err)?
                {
                    auto_title_job::ActiveModel {
                        conversation_id: Set(successor_conversation.id),
                        state: Set(auto_title_job::AutoTitleJobState::AwaitingTurn),
                        attempts: Set(0),
                        first_user_text: Set(source_job.first_user_text),
                        first_assistant_text: Set(None),
                        first_prompt_at: Set(source_job.first_prompt_at),
                        locale: Set(source_job.locale),
                        usable_turn_seq: Set(0),
                        attempt_turn_seq: Set(0),
                        last_usable_turn_token: Set(None),
                        config_gen: Set(source_job.config_gen),
                        updated_at: Set(now),
                    }
                    .insert(txn)
                    .await
                    .map_err(db_err)?;
                }

                if take_restart_header_failure() {
                    return Err(WorkflowStoreError::LegacyCompletionProtocolRestartRequired(
                        "injected successor header failure".into(),
                    ));
                }

                let successor_workflow_id = uuid::Uuid::new_v4().to_string();
                let document = restart_skeleton(
                    &successor_workflow_id,
                    &author_agent_type,
                    author_profile_id.as_deref(),
                );
                let normalized = validate_manifest_document(&document)?;
                let stored_document = normalized_to_document(&normalized);
                let document_json = serde_json::to_string(&stored_document).map_err(|error| {
                    WorkflowStoreError::LegacyCompletionProtocolRestartRequired(format!(
                        "serialize successor manifest: {error}"
                    ))
                })?;
                let document_digest = sha256_hex(document_json.as_bytes());
                let selection = CompletionProtocolSelection::legacy_restart();

                delegation_workflow::ActiveModel {
                    workflow_id: Set(successor_workflow_id.clone()),
                    parent_conversation_id: Set(successor_conversation.id),
                    workflow_kind: Set(WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY.into()),
                    schema_version: Set(i64::from(MANIFEST_SCHEMA_VERSION)),
                    active_manifest_revision: Set(1),
                    graph_revision: Set(1),
                    workflow_state: Set(delegation_workflow::WorkflowState::Skeleton),
                    capability_version: Set(WORKFLOW_CAPABILITY_VERSION.into()),
                    publication_token: Set(normalized.publication_token.clone()),
                    supersedes_approved_revision: Set(None),
                    structural_revision: Set(1),
                    design_fingerprint: Set(design_fingerprint_hash(&normalized)),
                    plan_fingerprint: Set(plan_fingerprint_hash(&normalized)),
                    block_cause_code: Set(None),
                    block_source_manifest_revision: Set(None),
                    completion_protocol_version: Set(selection.version),
                    completion_protocol_mode: Set(selection.mode),
                    legacy_source_workflow_id: Set(Some(source.workflow_id.clone())),
                    created_at: Set(now),
                    updated_at: Set(now),
                }
                .insert(txn)
                .await
                .map_err(restart_db_err)?;

                delegation_workflow_manifest_revision::ActiveModel {
                    workflow_id: Set(successor_workflow_id.clone()),
                    manifest_revision: Set(1),
                    manifest_state: Set("skeleton".into()),
                    document_json: Set(document_json),
                    document_digest: Set(document_digest),
                    revision_kind: Set(Some("publication".into())),
                    source_manifest_revision: Set(None),
                    recovery_authorization_id: Set(None),
                    transition_reason_code: Set(Some("legacy_protocol_restart".into())),
                    consumer_correlation_id: Set(None),
                    graph_revision: Set(Some(1)),
                    recovery_source_state_fingerprint: Set(None),
                    recovery_risk_class: Set(None),
                    created_at: Set(now),
                }
                .insert(txn)
                .await
                .map_err(restart_db_err)?;

                let author = normalized
                    .nodes
                    .iter()
                    .find(|node| node.role == Some(ManifestNodeRole::Author))
                    .expect("restart skeleton has a validated Plan Author");
                delegation_workflow_node_binding::ActiveModel {
                    workflow_id: Set(successor_workflow_id.clone()),
                    node_id: Set(author.id.clone()),
                    work_unit_key: Set(author
                        .work_unit_key
                        .clone()
                        .expect("validated author has work unit key")),
                    role: Set("author".into()),
                    agent_type: Set(author_agent_type),
                    profile_id: Set(author_profile_id),
                    phase_id: Set(PHASE_PLAN.into()),
                    task_index: Set(None),
                    introduced_revision: Set(1),
                    retired_revision: Set(None),
                    is_observed: Set(false),
                    retained_observed: Set(false),
                    cohort_frozen: Set(false),
                    node_outcome: Set(None),
                    created_at: Set(now),
                    updated_at: Set(now),
                }
                .insert(txn)
                .await
                .map_err(restart_db_err)?;

                Ok(RestartCommit {
                    source_workflow_id: source.workflow_id,
                    source_conversation_id,
                    successor_workflow_id,
                    successor_conversation_id: successor_conversation.id,
                    idempotent_replay: false,
                })
            })
        })
        .await
        .map_err(|error| match error {
            sea_orm::TransactionError::Transaction(error) => match error {
                WorkflowStoreError::ParentNotFound(_)
                | WorkflowStoreError::LegacyCompletionProtocolRestartInvalid(_)
                | WorkflowStoreError::LegacyCompletionProtocolRestartRequired(_) => error,
                other => {
                    WorkflowStoreError::LegacyCompletionProtocolRestartRequired(other.to_string())
                }
            },
            sea_orm::TransactionError::Connection(error) => {
                WorkflowStoreError::LegacyCompletionProtocolRestartRequired(error.to_string())
            }
        })?;

    let successor = delegation_workflow::Entity::find_by_id(&committed.successor_workflow_id)
        .one(&db.conn)
        .await
        .map_err(restart_db_err)?
        .ok_or_else(|| {
            WorkflowStoreError::LegacyCompletionProtocolRestartRequired(
                "committed successor is unavailable".into(),
            )
        })?;
    let completion_protocol = completion_protocol_projection(&db.conn, &successor)
        .await
        .map_err(restart_db_err)?;
    let restart_context = delegation_workflow_restart_context::Entity::find_by_id(
        committed.successor_conversation_id,
    )
    .one(&db.conn)
    .await
    .map_err(restart_db_err)?
    .ok_or_else(|| {
        WorkflowStoreError::LegacyCompletionProtocolRestartRequired(
            "committed successor restart context is unavailable".into(),
        )
    })?;
    Ok(LegacyWorkflowRestartProjection {
        source_workflow_id: committed.source_workflow_id,
        source_conversation_id: committed.source_conversation_id,
        successor_workflow_id: committed.successor_workflow_id,
        successor_conversation_id: committed.successor_conversation_id,
        open_gate: DocumentGateKind::Design,
        completion_protocol,
        restart_context: LegacyWorkflowRestartContext {
            original_conversation_id: restart_context.original_conversation_id,
            original_request_id: restart_context.original_request_id,
            original_request_text: restart_context.original_request_text,
            original_request_digest: restart_context.original_request_digest,
            agent_type: restart_context.agent_type,
            profile_id: restart_context.profile_id,
        },
        idempotent_replay: committed.idempotent_replay,
    })
}

/// Apply the current server-owned rollout policy before a caller mutates a
/// legacy root. Existing successors remain reusable after a rollout rollback;
/// creating a new successor requires the current selection to be enforce.
pub async fn restart_legacy_workflow_if_enforced(
    db: &AppDatabase,
    source_conversation_id: i32,
    rollout_subject: Option<(String, Option<String>)>,
    rollout: &super::types::CompletionProtocolRolloutConfig,
) -> Result<Option<LegacyWorkflowRestartProjection>, WorkflowStoreError> {
    let Some(source) = delegation_workflow::Entity::find()
        .filter(delegation_workflow::Column::ParentConversationId.eq(source_conversation_id))
        .filter(delegation_workflow::Column::WorkflowKind.eq(WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY))
        .one(&db.conn)
        .await
        .map_err(db_err)?
    else {
        return Ok(None);
    };
    if source.completion_protocol_version != 1 {
        return Ok(None);
    }

    let has_successor = delegation_workflow::Entity::find()
        .filter(delegation_workflow::Column::LegacySourceWorkflowId.eq(source.workflow_id.clone()))
        .one(&db.conn)
        .await
        .map_err(db_err)?
        .is_some();
    let (agent, profile) = match rollout_subject {
        Some(subject) => subject,
        None => delegation_workflow_node_binding::Entity::find()
            .filter(
                delegation_workflow_node_binding::Column::WorkflowId.eq(source.workflow_id.clone()),
            )
            .filter(delegation_workflow_node_binding::Column::Role.eq("author"))
            .one(&db.conn)
            .await
            .map_err(db_err)?
            .map(|binding| (binding.agent_type, binding.profile_id))
            .unwrap_or_else(|| ("unknown".into(), None)),
    };
    let selection = super::types::select_completion_protocol(&agent, profile.as_deref(), rollout);
    if !has_successor && selection.mode != delegation_workflow::CompletionProtocolMode::V2Enforce {
        return Ok(None);
    }

    restart_legacy_workflow_core(db, i64::from(source_conversation_id))
        .await
        .map(Some)
}

pub(crate) async fn completion_protocol_projection<C: ConnectionTrait>(
    conn: &C,
    header: &delegation_workflow::Model,
) -> Result<CompletionProtocolWorkflowProjection, sea_orm::DbErr> {
    let legacy_source = match header.legacy_source_workflow_id.as_deref() {
        Some(source_id) => delegation_workflow::Entity::find_by_id(source_id)
            .one(conn)
            .await?
            .map(|source| LegacyWorkflowLink {
                workflow_id: source.workflow_id,
                conversation_id: source.parent_conversation_id,
            }),
        None => None,
    };
    let v2_successor = delegation_workflow::Entity::find()
        .filter(delegation_workflow::Column::LegacySourceWorkflowId.eq(header.workflow_id.clone()))
        .one(conn)
        .await?
        .map(|successor| LegacyWorkflowLink {
            workflow_id: successor.workflow_id,
            conversation_id: successor.parent_conversation_id,
        });
    Ok(CompletionProtocolWorkflowProjection {
        version: header.completion_protocol_version,
        mode: header.completion_protocol_mode.clone(),
        creation_mode: header.completion_protocol_mode.clone(),
        legacy_source,
        read_only_reason: v2_successor
            .as_ref()
            .map(|_| LEGACY_RESTART_REASON.to_string()),
        v2_successor,
        // Restart deliberately creates a new root without re-entering an old
        // parent turn. The UI must offer explicit root-owned resume.
        automatic_root_wake: false,
    })
}

fn restart_skeleton(
    workflow_id: &str,
    author_agent_type: &str,
    author_profile_id: Option<&str>,
) -> ManifestDocument {
    let plan_path = format!("docs/superpowers/plans/restarted-{}.md", &workflow_id[..8]);
    let author_key = build_work_unit_key(&WorkUnitKeyParts::PlanAuthor {
        rel_plan_path: &plan_path,
        agent_type: author_agent_type,
        profile_id: author_profile_id,
    })
    .expect("generated restart Plan path is valid");
    ManifestDocument {
        schema_version: MANIFEST_SCHEMA_VERSION,
        workflow_kind: WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY.into(),
        plan_target_rel_path: plan_path,
        risk_policy_version: "b2d_task_risk_v1".into(),
        workflow_id: None,
        expected_manifest_revision: None,
        publication_token: format!("legacy-restart-{workflow_id}"),
        workflow_state: ManifestWorkflowState::Skeleton,
        design: None,
        plan: None,
        phases: vec![
            ManifestPhase {
                id: PHASE_DESIGN.into(),
                kind: Some(PHASE_DESIGN.into()),
                title: None,
            },
            ManifestPhase {
                id: PHASE_PLAN.into(),
                kind: Some(PHASE_PLAN.into()),
                title: None,
            },
        ],
        nodes: vec![
            ManifestNode {
                id: "design-root".into(),
                kind: ManifestNodeKind::Milestone,
                phase_id: Some(PHASE_DESIGN.into()),
                role: None,
                agent_type: None,
                profile_id: None,
                task_index: None,
                work_unit_key: None,
                deps: Vec::new(),
                required: Some(true),
                node_outcome: None,
                title: None,
            },
            ManifestNode {
                id: "plan-author".into(),
                kind: ManifestNodeKind::WorkUnit,
                phase_id: Some(PHASE_PLAN.into()),
                role: Some(ManifestNodeRole::Author),
                agent_type: Some(author_agent_type.into()),
                profile_id: author_profile_id.map(str::to_string),
                task_index: None,
                work_unit_key: Some(author_key),
                deps: vec!["design-root".into()],
                required: Some(true),
                node_outcome: None,
                title: None,
            },
        ],
        edges: Vec::new(),
        gates: Vec::new(),
        task_policies: Vec::new(),
    }
}

fn db_err(error: sea_orm::DbErr) -> WorkflowStoreError {
    WorkflowStoreError::Persistence(error.to_string())
}

fn restart_db_err(error: sea_orm::DbErr) -> WorkflowStoreError {
    WorkflowStoreError::LegacyCompletionProtocolRestartRequired(error.to_string())
}
