//! Durable identity and locator storage for Plan/progress-driven Simple workflows.

use std::collections::BTreeMap;

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect, Set, TransactionTrait,
};
use thiserror::Error;

use crate::db::entities::{
    conversation, delegation_task_run, delegation_workflow, delegation_workflow_run_binding,
    simple_workflow,
};

use super::key::{normalize_rel_path, parse_recognized_work_unit_key};
use super::types::WorkflowError;

const WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY: &str = "brainstorm_to_delivery";

#[derive(Debug, Clone, PartialEq)]
pub enum ConversationWorkflowMode {
    Ordinary {
        root_conversation_id: i32,
    },
    SimpleRegistered {
        root_conversation_id: i32,
        descriptor: simple_workflow::Model,
    },
    SimpleObserved {
        root_conversation_id: i32,
    },
    Archived {
        root_conversation_id: i32,
        workflow_id: String,
    },
    Corrupt {
        root_conversation_id: i32,
        workflow_id: String,
    },
}

impl ConversationWorkflowMode {
    pub const fn root_conversation_id(&self) -> i32 {
        match self {
            Self::Ordinary {
                root_conversation_id,
            }
            | Self::SimpleRegistered {
                root_conversation_id,
                ..
            }
            | Self::SimpleObserved {
                root_conversation_id,
            }
            | Self::Archived {
                root_conversation_id,
                ..
            }
            | Self::Corrupt {
                root_conversation_id,
                ..
            } => *root_conversation_id,
        }
    }

    pub const fn is_archived_or_corrupt(&self) -> bool {
        matches!(self, Self::Archived { .. } | Self::Corrupt { .. })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SimpleWorkflowRegistration {
    pub descriptor: simple_workflow::Model,
    pub created: bool,
    pub updated: bool,
}

#[derive(Debug, Error)]
pub enum SimpleWorkflowError {
    #[error(transparent)]
    Validation(#[from] WorkflowError),
    #[error("conversation {0} was not found")]
    ConversationNotFound(i32),
    #[error("conversation {parent_conversation_id} already owns archived workflow {workflow_id}")]
    ModeConflict {
        parent_conversation_id: i32,
        workflow_id: String,
    },
    #[error("conversation {parent_conversation_id} has conflicting workflow identities")]
    IdentityCorrupt { parent_conversation_id: i32 },
    #[error("simple workflow persistence failed: {0}")]
    Persistence(String),
}

impl SimpleWorkflowError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Validation(_) => "workflow_invalid_path",
            Self::ConversationNotFound(_) => "workflow_not_found",
            Self::ModeConflict { .. } | Self::IdentityCorrupt { .. } => "workflow_mode_conflict",
            Self::Persistence(_) => "workflow_persistence_failure",
        }
    }
}

fn db_error(error: sea_orm::DbErr) -> SimpleWorkflowError {
    SimpleWorkflowError::Persistence(error.to_string())
}

pub fn default_simple_progress_rel_path(parent_conversation_id: i32) -> String {
    format!(".superpowers/sdd/{parent_conversation_id}/progress.md")
}

pub async fn load_simple_workflow<C: ConnectionTrait>(
    conn: &C,
    parent_conversation_id: i32,
) -> Result<Option<simple_workflow::Model>, SimpleWorkflowError> {
    simple_workflow::Entity::find_by_id(parent_conversation_id)
        .one(conn)
        .await
        .map_err(db_error)
}

pub(crate) async fn register_simple_workflow_txn<C: ConnectionTrait>(
    conn: &C,
    parent_conversation_id: i32,
    plan_rel_path: &str,
    progress_rel_path: Option<&str>,
) -> Result<SimpleWorkflowRegistration, SimpleWorkflowError> {
    let parent = conversation::Entity::find_by_id(parent_conversation_id)
        .filter(conversation::Column::DeletedAt.is_null())
        .one(conn)
        .await
        .map_err(db_error)?
        .ok_or(SimpleWorkflowError::ConversationNotFound(
            parent_conversation_id,
        ))?;
    if parent.parent_id.is_some() {
        return Err(SimpleWorkflowError::ModeConflict {
            parent_conversation_id,
            workflow_id: "delegation_child".into(),
        });
    }

    let workflow = delegation_workflow::Entity::find()
        .filter(delegation_workflow::Column::ParentConversationId.eq(parent_conversation_id))
        .filter(delegation_workflow::Column::WorkflowKind.eq(WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY))
        .one(conn)
        .await
        .map_err(db_error)?;
    let existing = load_simple_workflow(conn, parent_conversation_id).await?;
    if let Some(workflow) = workflow {
        return Err(if existing.is_some() {
            SimpleWorkflowError::IdentityCorrupt {
                parent_conversation_id,
            }
        } else {
            SimpleWorkflowError::ModeConflict {
                parent_conversation_id,
                workflow_id: workflow.workflow_id,
            }
        });
    }

    let plan_rel_path = normalize_rel_path(plan_rel_path)?;
    let progress_rel_path = normalize_rel_path(
        progress_rel_path
            .map(str::to_owned)
            .unwrap_or_else(|| default_simple_progress_rel_path(parent_conversation_id))
            .as_str(),
    )?;

    let now = Utc::now();
    match existing {
        Some(current) => {
            let unchanged = current.plan_rel_path == plan_rel_path
                && current.progress_rel_path == progress_rel_path;
            if unchanged {
                return Ok(SimpleWorkflowRegistration {
                    descriptor: current,
                    created: false,
                    updated: false,
                });
            }
            let mut active: simple_workflow::ActiveModel = current.into();
            active.plan_rel_path = Set(plan_rel_path);
            active.progress_rel_path = Set(progress_rel_path);
            active.updated_at = Set(now);
            let descriptor = active.update(conn).await.map_err(db_error)?;
            Ok(SimpleWorkflowRegistration {
                descriptor,
                created: false,
                updated: true,
            })
        }
        None => {
            let descriptor = simple_workflow::ActiveModel {
                parent_conversation_id: Set(parent_conversation_id),
                plan_rel_path: Set(plan_rel_path),
                progress_rel_path: Set(progress_rel_path),
                created_at: Set(now),
                updated_at: Set(now),
            }
            .insert(conn)
            .await
            .map_err(db_error)?;
            Ok(SimpleWorkflowRegistration {
                descriptor,
                created: true,
                updated: false,
            })
        }
    }
}

pub async fn register_simple_workflow(
    conn: &DatabaseConnection,
    parent_conversation_id: i32,
    plan_rel_path: &str,
    progress_rel_path: Option<&str>,
) -> Result<SimpleWorkflowRegistration, SimpleWorkflowError> {
    let txn = conn.begin().await.map_err(db_error)?;
    let registration = register_simple_workflow_txn(
        &txn,
        parent_conversation_id,
        plan_rel_path,
        progress_rel_path,
    )
    .await?;
    txn.commit().await.map_err(db_error)?;
    Ok(registration)
}

async fn root_conversation_id<C: ConnectionTrait>(
    conn: &C,
    conversation_id: i32,
) -> Result<i32, SimpleWorkflowError> {
    let conversation = conversation::Entity::find_by_id(conversation_id)
        .filter(conversation::Column::DeletedAt.is_null())
        .one(conn)
        .await
        .map_err(db_error)?
        .ok_or(SimpleWorkflowError::ConversationNotFound(conversation_id))?;
    if conversation.parent_id.is_none() {
        return Ok(conversation.id);
    }

    let run_parent = delegation_task_run::Entity::find()
        .select_only()
        .column(delegation_task_run::Column::ParentConversationId)
        .filter(delegation_task_run::Column::ChildConversationId.eq(conversation_id))
        .order_by_desc(delegation_task_run::Column::Generation)
        .into_tuple::<i32>()
        .one(conn)
        .await
        .map_err(db_error)?;
    Ok(run_parent
        .or(conversation.parent_id)
        .unwrap_or(conversation.id))
}

async fn bound_workflows<C: ConnectionTrait>(
    conn: &C,
    conversation_id: i32,
) -> Result<BTreeMap<String, delegation_workflow::Model>, SimpleWorkflowError> {
    let task_ids = delegation_task_run::Entity::find()
        .select_only()
        .column(delegation_task_run::Column::TaskId)
        .filter(delegation_task_run::Column::ChildConversationId.eq(conversation_id))
        .into_tuple::<String>()
        .all(conn)
        .await
        .map_err(db_error)?;
    if task_ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let workflow_ids = delegation_workflow_run_binding::Entity::find()
        .select_only()
        .column(delegation_workflow_run_binding::Column::WorkflowId)
        .filter(delegation_workflow_run_binding::Column::TaskId.is_in(task_ids))
        .into_tuple::<String>()
        .all(conn)
        .await
        .map_err(db_error)?;
    if workflow_ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let rows = delegation_workflow::Entity::find()
        .filter(delegation_workflow::Column::WorkflowId.is_in(workflow_ids))
        .all(conn)
        .await
        .map_err(db_error)?;
    Ok(rows
        .into_iter()
        .map(|row| (row.workflow_id.clone(), row))
        .collect())
}

pub async fn resolve_conversation_workflow_mode<C: ConnectionTrait>(
    conn: &C,
    conversation_id: i32,
) -> Result<ConversationWorkflowMode, SimpleWorkflowError> {
    let fallback_root = root_conversation_id(conn, conversation_id).await?;
    let mut workflows = bound_workflows(conn, conversation_id).await?;
    let root_workflows = delegation_workflow::Entity::find()
        .filter(delegation_workflow::Column::ParentConversationId.eq(fallback_root))
        .filter(delegation_workflow::Column::WorkflowKind.eq(WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY))
        .all(conn)
        .await
        .map_err(db_error)?;
    for row in root_workflows {
        workflows.insert(row.workflow_id.clone(), row);
    }

    if let Some((workflow_id, workflow)) = workflows.iter().next() {
        let root_conversation_id = workflow.parent_conversation_id;
        let descriptor = load_simple_workflow(conn, root_conversation_id).await?;
        if workflows.len() > 1 || descriptor.is_some() {
            return Ok(ConversationWorkflowMode::Corrupt {
                root_conversation_id,
                workflow_id: workflow_id.clone(),
            });
        }
        return Ok(ConversationWorkflowMode::Archived {
            root_conversation_id,
            workflow_id: workflow_id.clone(),
        });
    }

    if let Some(descriptor) = load_simple_workflow(conn, fallback_root).await? {
        return Ok(ConversationWorkflowMode::SimpleRegistered {
            root_conversation_id: fallback_root,
            descriptor,
        });
    }

    let work_unit_keys = delegation_task_run::Entity::find()
        .select_only()
        .column(delegation_task_run::Column::WorkUnitKey)
        .filter(delegation_task_run::Column::ParentConversationId.eq(fallback_root))
        .filter(delegation_task_run::Column::WorkUnitKey.is_not_null())
        .into_tuple::<Option<String>>()
        .all(conn)
        .await
        .map_err(db_error)?;
    if work_unit_keys
        .into_iter()
        .flatten()
        .any(|key| parse_recognized_work_unit_key(&key).is_some())
    {
        return Ok(ConversationWorkflowMode::SimpleObserved {
            root_conversation_id: fallback_root,
        });
    }

    Ok(ConversationWorkflowMode::Ordinary {
        root_conversation_id: fallback_root,
    })
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use sea_orm::{ActiveModelTrait, Set};

    use super::{
        load_simple_workflow, register_simple_workflow, resolve_conversation_workflow_mode,
        ConversationWorkflowMode,
    };
    use crate::db::entities::delegation_task_run::{self, AdmissionClass, DelegationRunStatus};
    use crate::db::entities::delegation_workflow::{self, CompletionProtocolMode, WorkflowState};
    use crate::db::entities::delegation_workflow_run_binding;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
    use crate::db::AppDatabase;
    use crate::models::agent::AgentType;

    async fn seed_workflow(db: &AppDatabase, parent: i32, workflow_id: &str) {
        let now = Utc::now();
        delegation_workflow::ActiveModel {
            workflow_id: Set(workflow_id.into()),
            parent_conversation_id: Set(parent),
            workflow_kind: Set("brainstorm_to_delivery".into()),
            schema_version: Set(1),
            active_manifest_revision: Set(1),
            graph_revision: Set(1),
            workflow_state: Set(WorkflowState::Approved),
            capability_version: Set("workflow_manifest_v1".into()),
            publication_token: Set(format!("publication-{workflow_id}")),
            supersedes_approved_revision: Set(None),
            structural_revision: Set(1),
            design_fingerprint: Set("design".into()),
            plan_fingerprint: Set("plan".into()),
            block_cause_code: Set(None),
            block_source_manifest_revision: Set(None),
            completion_protocol_version: Set(2),
            completion_protocol_mode: Set(CompletionProtocolMode::V2Enforce),
            legacy_source_workflow_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&db.conn)
        .await
        .expect("workflow header");
    }

    async fn seed_recognized_run(db: &AppDatabase, parent: i32, child: i32, task_id: &str) {
        let now = Utc::now();
        delegation_task_run::ActiveModel {
            task_id: Set(task_id.into()),
            root_task_id: Set(task_id.into()),
            previous_task_id: Set(None),
            generation: Set(1),
            parent_conversation_id: Set(parent),
            parent_tool_use_id: Set(None),
            child_conversation_id: Set(child),
            agent_type: Set("codex".into()),
            profile_id: Set(None),
            workspace_path: Set(None),
            route_fingerprint: Set(None),
            launch_snapshot_version: Set(None),
            mode_id: Set(None),
            config_values_json: Set(None),
            task_preview: Set(None),
            request_fingerprint: Set(None),
            admission_class: Set(AdmissionClass::NormalRevision),
            reached_running_at: Set(Some(now)),
            lineage_root_task_id: Set(task_id.into()),
            work_unit_key: Set(Some("task|1|implementer|codex|none".into())),
            legacy_parent_tool_use_id: Set(None),
            history_only: Set(false),
            status: Set(DelegationRunStatus::Completed),
            error_code: Set(None),
            termination_audit_json: Set(None),
            started_at: Set(Some(now)),
            finished_at: Set(Some(now)),
            tool_call_count: Set(None),
            edit_tool_call_count: Set(None),
            touched_files_json: Set(None),
            touched_files_truncated: Set(None),
            additions: Set(None),
            deletions: Set(None),
            line_counts_complete: Set(None),
            card_summary_json: Set(None),
            child_turn_anchor: Set(None),
            child_connection_id: Set(None),
            replaced_task_id: Set(None),
            replacement_reason: Set(None),
            recovery_authorization_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(&db.conn)
        .await
        .expect("recognized run");
    }

    #[tokio::test]
    async fn simple_workflow_store_normalizes_and_updates_locators_idempotently() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/simple-register").await;
        let parent = seed_conversation(&db, folder, AgentType::Codex).await;

        let created =
            register_simple_workflow(&db.conn, parent, "./docs//superpowers/plans/plan.md", None)
                .await
                .expect("register");
        assert!(created.created);
        assert!(!created.updated);
        assert_eq!(
            created.descriptor.plan_rel_path,
            "docs/superpowers/plans/plan.md"
        );
        assert_eq!(
            created.descriptor.progress_rel_path,
            format!(".superpowers/sdd/{parent}/progress.md")
        );
        let original_created_at = created.descriptor.created_at;

        let replay =
            register_simple_workflow(&db.conn, parent, "docs/superpowers/plans/plan.md", None)
                .await
                .expect("idempotent replay");
        assert!(!replay.created);
        assert!(!replay.updated);
        assert_eq!(replay.descriptor.created_at, original_created_at);

        let updated = register_simple_workflow(
            &db.conn,
            parent,
            "docs/superpowers/plans/revised.md",
            Some("./.superpowers//sdd/custom/progress.md"),
        )
        .await
        .expect("locator update");
        assert!(!updated.created);
        assert!(updated.updated);
        assert_eq!(
            updated.descriptor.plan_rel_path,
            "docs/superpowers/plans/revised.md"
        );
        assert_eq!(
            updated.descriptor.progress_rel_path,
            ".superpowers/sdd/custom/progress.md"
        );
        assert_eq!(updated.descriptor.created_at, original_created_at);
        assert_eq!(
            load_simple_workflow(&db.conn, parent)
                .await
                .expect("load")
                .expect("descriptor"),
            updated.descriptor
        );
    }

    #[tokio::test]
    async fn simple_workflow_store_rejects_archived_parent_without_writing() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/simple-archived-conflict").await;
        let parent = seed_conversation(&db, folder, AgentType::Codex).await;
        seed_workflow(&db, parent, "workflow-archived").await;

        let error = register_simple_workflow(&db.conn, parent, "docs/plan.md", None)
            .await
            .expect_err("archived identity must reject registration");
        assert_eq!(error.code(), "workflow_mode_conflict");
        assert!(load_simple_workflow(&db.conn, parent)
            .await
            .expect("load")
            .is_none());
    }

    #[tokio::test]
    async fn simple_workflow_store_resolves_registered_observed_archived_and_corrupt_modes() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/simple-mode-resolution").await;

        let registered = seed_conversation(&db, folder, AgentType::Codex).await;
        register_simple_workflow(&db.conn, registered, "docs/registered.md", None)
            .await
            .expect("register");
        assert!(matches!(
            resolve_conversation_workflow_mode(&db.conn, registered)
                .await
                .expect("registered mode"),
            ConversationWorkflowMode::SimpleRegistered { .. }
        ));

        let observed = seed_conversation(&db, folder, AgentType::Codex).await;
        let observed_child = seed_conversation(&db, folder, AgentType::Codex).await;
        seed_recognized_run(&db, observed, observed_child, "observed-task").await;
        assert_eq!(
            resolve_conversation_workflow_mode(&db.conn, observed)
                .await
                .expect("observed mode"),
            ConversationWorkflowMode::SimpleObserved {
                root_conversation_id: observed
            }
        );

        let archived = seed_conversation(&db, folder, AgentType::Codex).await;
        let archived_child = seed_conversation(&db, folder, AgentType::Codex).await;
        seed_recognized_run(&db, archived, archived_child, "archived-task").await;
        seed_workflow(&db, archived, "workflow-priority").await;
        delegation_workflow_run_binding::ActiveModel {
            task_id: Set("archived-task".into()),
            workflow_id: Set("workflow-priority".into()),
            node_id: Set("task-1-implementer".into()),
            gate_id: Set(None),
            gate_cycle: Set(None),
            manifest_revision: Set(1),
            lineage_ordinal: Set(1),
            summary_validated: Set(false),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(&db.conn)
        .await
        .expect("run binding");
        assert!(matches!(
            resolve_conversation_workflow_mode(&db.conn, archived)
                .await
                .expect("archived root"),
            ConversationWorkflowMode::Archived {
                ref workflow_id,
                root_conversation_id
            } if workflow_id == "workflow-priority" && root_conversation_id == archived
        ));
        assert!(matches!(
            resolve_conversation_workflow_mode(&db.conn, archived_child)
                .await
                .expect("archived child"),
            ConversationWorkflowMode::Archived {
                ref workflow_id,
                root_conversation_id
            } if workflow_id == "workflow-priority" && root_conversation_id == archived
        ));

        let corrupt = seed_conversation(&db, folder, AgentType::Codex).await;
        register_simple_workflow(&db.conn, corrupt, "docs/corrupt.md", None)
            .await
            .expect("register before historical conflict");
        seed_workflow(&db, corrupt, "workflow-corrupt").await;
        assert!(matches!(
            resolve_conversation_workflow_mode(&db.conn, corrupt)
                .await
                .expect("corrupt mode"),
            ConversationWorkflowMode::Corrupt {
                ref workflow_id,
                root_conversation_id
            } if workflow_id == "workflow-corrupt" && root_conversation_id == corrupt
        ));
    }
}
