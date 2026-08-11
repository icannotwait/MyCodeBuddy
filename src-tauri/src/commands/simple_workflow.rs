use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QuerySelect, TransactionTrait,
};
use serde::{Deserialize, Serialize};

use crate::acp::delegation::route::DelegationRoutePolicy;
use crate::acp::delegation::store::{
    classify_sqlite_transient, classify_sqlite_transient_msg, extract_sqlite_codes,
};
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
    conversation, delegation_task_run, delegation_workflow,
    delegation_workflow_manifest_revision, delegation_workflow_run_binding, folder,
    simple_workflow,
};
use crate::db::error::DbError;
use crate::db::service::conversation_service;
use crate::db::AppDatabase;
use crate::models::AgentType;
use crate::web::event_bridge::EventEmitter;

const MAX_CLIENT_REQUEST_TOKEN_BYTES: usize = 256;
const MAX_SIMPLE_SUCCESSOR_LOCATOR_BYTES: usize = 4 * 1024;
const MAX_SIMPLE_SUCCESSOR_BOOTSTRAP_BYTES: usize = 16 * 1024;
const SIMPLE_SUCCESSOR_TXN_MAX_ATTEMPTS: u8 = 10;
const SIMPLE_SUCCESSOR_SOURCE_NOT_ARCHIVED_MESSAGE: &str =
    "Source conversation is not an archived workflow";
const SIMPLE_SUCCESSOR_SOURCE_ALREADY_SIMPLE_MESSAGE: &str =
    "Source conversation already uses Simple";

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

#[cfg(test)]
struct SimpleSuccessorTestControl {
    empty_link_barrier: Option<std::sync::Arc<tokio::sync::Barrier>>,
    empty_link_slots: std::sync::atomic::AtomicUsize,
    fail_after_registration: std::sync::atomic::AtomicUsize,
    empty_link_waits: std::sync::atomic::AtomicUsize,
    retries: std::sync::atomic::AtomicUsize,
    rollbacks: std::sync::atomic::AtomicUsize,
    auto_title_job_seen: std::sync::atomic::AtomicBool,
    descriptor_link_seen: std::sync::atomic::AtomicBool,
}

#[cfg(test)]
impl SimpleSuccessorTestControl {
    async fn after_empty_link_read(&self) {
        let claimed = self
            .empty_link_slots
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |remaining| (remaining > 0).then(|| remaining - 1),
            )
            .is_ok();
        if claimed {
            self.empty_link_waits
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if let Some(barrier) = self.empty_link_barrier.as_ref() {
                barrier.wait().await;
            }
        }
    }

    fn should_fail_after_registration(&self) -> bool {
        self.fail_after_registration
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |remaining| (remaining > 0).then(|| remaining - 1),
            )
            .is_ok()
    }

    fn record_retry(&self) {
        self.retries
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    fn record_rollback(&self) {
        self.rollbacks
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    fn new(empty_link_parties: Option<usize>, fail_after_registration: usize) -> Self {
        Self {
            empty_link_barrier: empty_link_parties
                .map(|parties| std::sync::Arc::new(tokio::sync::Barrier::new(parties))),
            empty_link_slots: std::sync::atomic::AtomicUsize::new(
                empty_link_parties.unwrap_or(0),
            ),
            fail_after_registration: std::sync::atomic::AtomicUsize::new(
                fail_after_registration,
            ),
            empty_link_waits: std::sync::atomic::AtomicUsize::new(0),
            retries: std::sync::atomic::AtomicUsize::new(0),
            rollbacks: std::sync::atomic::AtomicUsize::new(0),
            auto_title_job_seen: std::sync::atomic::AtomicBool::new(false),
            descriptor_link_seen: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn snapshot_race(parties: usize) -> Self {
        Self::new(Some(parties), 0)
    }

    fn fail_after_registration() -> Self {
        Self::new(None, 1)
    }

    fn empty_link_waits(&self) -> usize {
        self.empty_link_waits
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    fn retries(&self) -> usize {
        self.retries.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn rollbacks(&self) -> usize {
        self.rollbacks.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn record_auto_title_job_seen(&self, seen: bool) {
        self.auto_title_job_seen
            .store(seen, std::sync::atomic::Ordering::SeqCst);
    }

    fn auto_title_job_seen(&self) -> bool {
        self.auto_title_job_seen
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    fn record_descriptor_link_seen(&self, seen: bool) {
        self.descriptor_link_seen
            .store(seen, std::sync::atomic::Ordering::SeqCst);
    }

    fn descriptor_link_seen(&self) -> bool {
        self.descriptor_link_seen
            .load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(test)]
tokio::task_local! {
    static SIMPLE_SUCCESSOR_TEST_CONTROL: std::sync::Arc<SimpleSuccessorTestControl>;
}

#[cfg(test)]
async fn test_after_empty_link_read() {
    if let Ok(control) = SIMPLE_SUCCESSOR_TEST_CONTROL.try_with(|control| control.clone()) {
        control.after_empty_link_read().await;
    }
}

#[cfg(test)]
fn test_should_fail_after_registration() -> bool {
    SIMPLE_SUCCESSOR_TEST_CONTROL
        .try_with(|control| control.should_fail_after_registration())
        .unwrap_or(false)
}

#[cfg(test)]
fn test_record_retry() {
    let _ = SIMPLE_SUCCESSOR_TEST_CONTROL.try_with(|control| control.record_retry());
}

#[cfg(test)]
fn test_record_rollback() {
    let _ = SIMPLE_SUCCESSOR_TEST_CONTROL.try_with(|control| control.record_rollback());
}

#[cfg(test)]
async fn test_record_candidate_registration(
    txn: &sea_orm::DatabaseTransaction,
    conversation_id: i32,
    source_workflow_id: &str,
) -> Result<(), AppCommandError> {
    let Ok(control) = SIMPLE_SUCCESSOR_TEST_CONTROL.try_with(|control| control.clone()) else {
        return Ok(());
    };
    let seen = crate::db::entities::auto_title_job::Entity::find_by_id(conversation_id)
        .one(txn)
        .await
        .map_err(|error| AppCommandError::database_error(error.to_string()))?
        .is_some();
    control.record_auto_title_job_seen(seen);
    let descriptor = simple_workflow::Entity::find_by_id(conversation_id)
        .one(txn)
        .await
        .map_err(|error| AppCommandError::database_error(error.to_string()))?;
    control.record_descriptor_link_seen(descriptor.is_some_and(|descriptor| {
        descriptor.source_workflow_id.as_deref() == Some(source_workflow_id)
    }));
    Ok(())
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
    match normalize_bounded_successor_locator(plan_rel_path) {
        Some(safe_rel_path) => error.with_detail(safe_rel_path),
        None => error,
    }
}

fn normalize_bounded_successor_locator(rel_path: &str) -> Option<String> {
    if rel_path.len() > MAX_SIMPLE_SUCCESSOR_LOCATOR_BYTES {
        return None;
    }
    let normalized = normalize_rel_path(rel_path).ok()?;
    (normalized.len() <= MAX_SIMPLE_SUCCESSOR_LOCATOR_BYTES).then_some(normalized)
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
    let prompt = lines.join("\n");
    debug_assert!(prompt.len() <= MAX_SIMPLE_SUCCESSOR_BOOTSTRAP_BYTES);
    prompt
}

fn source_not_archived() -> AppCommandError {
    AppCommandError::new(
        AppErrorCode::SimpleSuccessorSourceNotArchived,
        SIMPLE_SUCCESSOR_SOURCE_NOT_ARCHIVED_MESSAGE,
    )
}

fn source_already_simple() -> AppCommandError {
    AppCommandError::new(
        AppErrorCode::SimpleSuccessorSourceAlreadySimple,
        SIMPLE_SUCCESSOR_SOURCE_ALREADY_SIMPLE_MESSAGE,
    )
}

async fn is_durably_bound_archived_child(
    db: &AppDatabase,
    source_conversation_id: i32,
    root_conversation_id: i32,
    workflow_id: &str,
) -> Result<bool, AppCommandError> {
    if source_conversation_id == root_conversation_id {
        return Ok(true);
    }
    let task_ids = delegation_task_run::Entity::find()
        .select_only()
        .column(delegation_task_run::Column::TaskId)
        .filter(
            delegation_task_run::Column::ChildConversationId.eq(source_conversation_id),
        )
        .into_tuple::<String>()
        .all(&db.conn)
        .await
        .map_err(|error| AppCommandError::database_error(error.to_string()))?;
    if task_ids.is_empty() {
        return Ok(false);
    }
    delegation_workflow_run_binding::Entity::find()
        .filter(delegation_workflow_run_binding::Column::TaskId.is_in(task_ids))
        .filter(delegation_workflow_run_binding::Column::WorkflowId.eq(workflow_id))
        .one(&db.conn)
        .await
        .map(|binding| binding.is_some())
        .map_err(|error| AppCommandError::database_error(error.to_string()))
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
        } => {
            if !is_durably_bound_archived_child(
                db,
                source_conversation_id,
                root_conversation_id,
                &workflow_id,
            )
            .await?
            {
                return Err(source_not_archived());
            }
            (root_conversation_id, workflow_id)
        }
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
        ConversationWorkflowMode::Ordinary { .. } => return Err(source_not_archived()),
        ConversationWorkflowMode::SimpleRegistered { .. }
        | ConversationWorkflowMode::SimpleObserved { .. } => return Err(source_already_simple()),
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
    let plan_rel_path = normalize_bounded_successor_locator(&raw_plan_rel_path)
        .ok_or_else(|| plan_unavailable(&raw_plan_rel_path))?;
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
        .and_then(|design| normalize_bounded_successor_locator(&design.rel_path));

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
    if normalize_bounded_successor_locator(&descriptor.progress_rel_path)
        .map_or(true, |normalized| normalized != descriptor.progress_rel_path)
    {
        return Err(AppCommandError::new(
            AppErrorCode::WorkflowIdentityCorrupt,
            "Simple successor progress locator is invalid",
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

fn is_unique_successor_conflict_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("unique constraint")
        || lower.contains("idx_simple_workflows_source")
        || lower.contains("2067")
}

fn is_retryable_successor_conflict_message(message: &str) -> bool {
    classify_sqlite_transient_msg(message).is_some()
        || is_unique_successor_conflict_message(message)
}

fn is_retryable_successor_db_err(error: &sea_orm::DbErr) -> bool {
    const SQLITE_CONSTRAINT: i32 = 19;
    const SQLITE_CONSTRAINT_PRIMARYKEY: i32 = 1555;
    const SQLITE_CONSTRAINT_UNIQUE: i32 = 2067;

    classify_sqlite_transient(error).is_some()
        || extract_sqlite_codes(error).is_some_and(|codes| {
            codes.primary == SQLITE_CONSTRAINT
                && matches!(
                    codes.extended,
                    SQLITE_CONSTRAINT_PRIMARYKEY | SQLITE_CONSTRAINT_UNIQUE
                )
        })
        || is_unique_successor_conflict_message(&error.to_string())
}

fn is_retryable_successor_db_error(error: &DbError) -> bool {
    match error {
        DbError::Database(error) => is_retryable_successor_db_err(error),
        _ => false,
    }
}

fn is_retryable_successor_app_error(error: &AppCommandError) -> bool {
    error.code == AppErrorCode::DatabaseError
        && is_retryable_successor_conflict_message(&error.message)
}

fn is_retryable_successor_simple_error(error: &SimpleWorkflowError) -> bool {
    matches!(
        error,
        SimpleWorkflowError::Persistence(message)
            if is_retryable_successor_conflict_message(message)
    )
}

fn successor_retry_delay(attempt: u8) -> std::time::Duration {
    std::time::Duration::from_millis(5 + u64::from(attempt) * 10)
}

async fn rollback_successor_attempt(
    txn: sea_orm::DatabaseTransaction,
) -> Result<(), AppCommandError> {
    txn.rollback()
        .await
        .map_err(|error| AppCommandError::database_error(error.to_string()))?;
    #[cfg(test)]
    test_record_rollback();
    Ok(())
}

async fn converge_successor_conflict(
    db: &AppDatabase,
    source: &ArchivedSource,
    attempt: u8,
    error: AppCommandError,
) -> Result<Option<SimpleSuccessorResult>, AppCommandError> {
    #[cfg(test)]
    test_record_retry();
    tokio::time::sleep(successor_retry_delay(attempt)).await;
    match load_existing_successor(&db.conn, source).await {
        Ok(Some(existing)) => Ok(Some(existing)),
        Ok(None) if attempt < SIMPLE_SUCCESSOR_TXN_MAX_ATTEMPTS => Ok(None),
        Ok(None) => Err(error),
        Err(fresh_error)
            if is_retryable_successor_app_error(&fresh_error)
                && attempt < SIMPLE_SUCCESSOR_TXN_MAX_ATTEMPTS =>
        {
            Ok(None)
        }
        Err(fresh_error) if is_retryable_successor_app_error(&fresh_error) => Err(error),
        Err(fresh_error) => Err(fresh_error),
    }
}

async fn create_or_load_successor(
    db: &AppDatabase,
    source: &ArchivedSource,
) -> Result<SimpleSuccessorResult, AppCommandError> {
    for attempt in 1..=SIMPLE_SUCCESSOR_TXN_MAX_ATTEMPTS {
        match load_existing_successor(&db.conn, source).await {
            Ok(Some(existing)) => return Ok(existing),
            Ok(None) => {}
            Err(error) if is_retryable_successor_app_error(&error) => {
                if let Some(existing) =
                    converge_successor_conflict(db, source, attempt, error).await?
                {
                    return Ok(existing);
                }
                continue;
            }
            Err(error) => return Err(error),
        }

        let txn = match db.conn.begin().await {
            Ok(txn) => txn,
            Err(error) if is_retryable_successor_db_err(&error) => {
                let error = AppCommandError::database_error(error.to_string());
                if let Some(existing) =
                    converge_successor_conflict(db, source, attempt, error).await?
                {
                    return Ok(existing);
                }
                continue;
            }
            Err(error) => return Err(AppCommandError::database_error(error.to_string())),
        };
        match load_existing_successor(&txn, source).await {
            Ok(Some(existing)) => match txn.commit().await {
                Ok(()) => return Ok(existing),
                Err(error) if is_retryable_successor_db_err(&error) => {
                    let error = AppCommandError::database_error(error.to_string());
                    if let Some(existing) =
                        converge_successor_conflict(db, source, attempt, error)
                            .await?
                    {
                        return Ok(existing);
                    }
                    continue;
                }
                Err(error) => return Err(AppCommandError::database_error(error.to_string())),
            },
            Ok(None) => {}
            Err(error) => {
                let retryable = is_retryable_successor_app_error(&error);
                rollback_successor_attempt(txn).await?;
                if retryable {
                    if let Some(existing) =
                        converge_successor_conflict(db, source, attempt, error)
                            .await?
                    {
                        return Ok(existing);
                    }
                    continue;
                }
                return Err(error);
            }
        }
        #[cfg(test)]
        test_after_empty_link_read().await;

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
                let retryable = is_retryable_successor_db_error(&error);
                let error = AppCommandError::from(error);
                rollback_successor_attempt(txn).await?;
                if retryable {
                    if let Some(existing) =
                        converge_successor_conflict(db, source, attempt, error)
                            .await?
                    {
                        return Ok(existing);
                    }
                    continue;
                }
                return Err(error);
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
            let retryable = is_retryable_successor_simple_error(&error);
            let error = simple_error(error);
            rollback_successor_attempt(txn).await?;
            if retryable {
                if let Some(existing) =
                    converge_successor_conflict(db, source, attempt, error).await?
                {
                    return Ok(existing);
                }
                continue;
            }
            return Err(error);
        }
        #[cfg(test)]
        test_record_candidate_registration(&txn, candidate.id, &source.workflow_id).await?;
        #[cfg(test)]
        if test_should_fail_after_registration() {
            rollback_successor_attempt(txn).await?;
            return Err(AppCommandError::database_error(
                "forced Simple successor failure after descriptor registration",
            ));
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
            Err(error) if is_retryable_successor_db_err(&error) => {
                let error = AppCommandError::database_error(error.to_string());
                if let Some(existing) =
                    converge_successor_conflict(db, source, attempt, error).await?
                {
                    return Ok(existing);
                }
                continue;
            }
            Err(error) => {
                return Err(AppCommandError::database_error(error.to_string()));
            }
        }
    }
    Err(AppCommandError::database_error(
        "Simple successor creation did not converge",
    ))
}

#[cfg(test)]
async fn create_or_load_successor_controlled(
    db: &AppDatabase,
    source: &ArchivedSource,
    control: std::sync::Arc<SimpleSuccessorTestControl>,
) -> Result<SimpleSuccessorResult, AppCommandError> {
    SIMPLE_SUCCESSOR_TEST_CONTROL
        .scope(control, create_or_load_successor(db, source))
        .await
}

#[cfg(test)]
async fn continue_archived_workflow_in_simple_controlled(
    db: &AppDatabase,
    source_conversation_id: i32,
    client_request_token: &str,
    control: std::sync::Arc<SimpleSuccessorTestControl>,
) -> Result<SimpleSuccessorResult, AppCommandError> {
    validate_request_token(client_request_token)?;
    let source = load_archived_source(db, source_conversation_id).await?;
    create_or_load_successor_controlled(db, &source, control).await
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
    use crate::auto_title::{enable_title_api_for_test, title_key};
    use crate::commands::conversations::delete_conversation_with_cleanup_core;
    use crate::db::entities::delegation_workflow::CompletionProtocolMode;
    use crate::db::entities::{
        auto_title_job, conversation, delegation_task_run, delegation_workflow,
        delegation_workflow_manifest_revision, delegation_workflow_run_binding, simple_workflow,
    };
    use crate::db::service::conversation_service;
    use crate::db::test_helpers::{
        fresh_disk_db, fresh_in_memory_db, seed_conversation, seed_folder,
    };
    use crate::models::AgentType;
    use crate::acp::delegation::spawner::DelegationLink;
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

    #[cfg(windows)]
    fn locator_that_expands_past_successor_bound() -> String {
        let raw = format!("docs/{}a", "\u{0130}".repeat(2045));
        assert_eq!(raw.len(), MAX_SIMPLE_SUCCESSOR_LOCATOR_BYTES);
        assert_eq!(normalize_rel_path(&raw).unwrap().len(), 6141);
        raw
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
    async fn simple_successor_rejects_unbound_child_of_archived_root_without_writes() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace.path().join("docs")).unwrap();
        std::fs::write(workspace.path().join("docs/plan.md"), "# Plan\n").unwrap();
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, workspace.path().to_str().unwrap()).await;
        let root = seed_conversation(&db, folder, AgentType::Codex).await;
        seed_archived_workflow(
            &db,
            root,
            "workflow-unbound-child",
            "docs/plan.md",
            None,
            2,
            CompletionProtocolMode::V2Enforce,
        )
        .await;
        let child = conversation_service::create_with_delegation(
            &db.conn,
            folder,
            AgentType::Codex,
            None,
            None,
            Some(DelegationLink {
                parent_conversation_id: root,
                parent_tool_use_id: "unbound-parent-tool".into(),
                delegation_call_id: "unbound-call".into(),
            }),
        )
        .await
        .unwrap();
        let conversations_before = live_conversation_count(&db).await;
        let descriptors_before = descriptor_count(&db).await;
        let title_jobs_before = auto_title_job::Entity::find()
            .count(&db.conn)
            .await
            .unwrap();

        let error = continue_archived_workflow_in_simple_core(
            &db,
            &EventEmitter::Noop,
            child.id,
            "unbound-child-request",
        )
        .await
        .unwrap_err();

        assert_eq!(error.code, AppErrorCode::SimpleSuccessorSourceNotArchived);
        assert_eq!(error.message, SIMPLE_SUCCESSOR_SOURCE_NOT_ARCHIVED_MESSAGE);
        assert_eq!(live_conversation_count(&db).await, conversations_before);
        assert_eq!(descriptor_count(&db).await, descriptors_before);
        assert_eq!(
            auto_title_job::Entity::find().count(&db.conn).await.unwrap(),
            title_jobs_before
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
        assert_eq!(
            ordinary_error.code,
            AppErrorCode::SimpleSuccessorSourceNotArchived
        );
        assert_eq!(
            ordinary_error.message,
            SIMPLE_SUCCESSOR_SOURCE_NOT_ARCHIVED_MESSAGE
        );

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
        assert_eq!(
            simple_error.code,
            AppErrorCode::SimpleSuccessorSourceAlreadySimple
        );
        assert_eq!(
            simple_error.message,
            SIMPLE_SUCCESSOR_SOURCE_ALREADY_SIMPLE_MESSAGE
        );

        let observed = seed_conversation(&db, folder, AgentType::Codex).await;
        let observed_child = seed_conversation(&db, folder, AgentType::Codex).await;
        let now = chrono::Utc::now();
        delegation_task_run::ActiveModel {
            task_id: Set("observed-simple-task".into()),
            root_task_id: Set("observed-simple-task".into()),
            previous_task_id: Set(None),
            generation: Set(1),
            parent_conversation_id: Set(observed),
            parent_tool_use_id: Set(None),
            child_conversation_id: Set(observed_child),
            agent_type: Set("codex".into()),
            admission_class: Set(
                crate::db::entities::delegation_task_run::AdmissionClass::NormalRevision,
            ),
            lineage_root_task_id: Set("observed-simple-task".into()),
            work_unit_key: Set(Some("task|1|implementer|codex|none".into())),
            history_only: Set(false),
            status: Set(crate::db::entities::delegation_task_run::DelegationRunStatus::Completed),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(&db.conn)
        .await
        .unwrap();
        let observed_error = continue_archived_workflow_in_simple_core(
            &db,
            &EventEmitter::Noop,
            observed,
            "observed-simple-request",
        )
        .await
        .unwrap_err();
        assert_eq!(
            observed_error.code,
            AppErrorCode::SimpleSuccessorSourceAlreadySimple
        );
        assert_eq!(
            observed_error.message,
            SIMPLE_SUCCESSOR_SOURCE_ALREADY_SIMPLE_MESSAGE
        );

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

    #[cfg(windows)]
    #[tokio::test]
    async fn simple_successor_rejects_plan_locator_oversized_after_normalization() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace.path().join("docs")).unwrap();
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, workspace.path().to_str().unwrap()).await;
        let source = seed_conversation(&db, folder, AgentType::Codex).await;
        let plan_rel_path = locator_that_expands_past_successor_bound();
        seed_archived_workflow(
            &db,
            source,
            "workflow-normalized-oversized-plan-locator",
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
            "normalized-oversized-plan-request",
        )
        .await
        .unwrap_err();

        assert_eq!(error.code, AppErrorCode::SimpleSuccessorPlanUnavailable);
        assert_eq!(error.detail, None);
        assert_eq!(live_conversation_count(&db).await, conversations_before);
        assert_eq!(descriptor_count(&db).await, descriptors_before);
    }

    #[tokio::test]
    async fn simple_successor_omits_oversized_design_locator_from_bootstrap() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace.path().join("docs")).unwrap();
        std::fs::write(workspace.path().join("docs/plan.md"), "# Plan\n").unwrap();
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, workspace.path().to_str().unwrap()).await;
        let source = seed_conversation(&db, folder, AgentType::Codex).await;
        seed_archived_workflow(
            &db,
            source,
            "workflow-oversized-design-locator",
            "docs/plan.md",
            Some("docs/design.md"),
            2,
            CompletionProtocolMode::V2Enforce,
        )
        .await;
        let revision = delegation_workflow_manifest_revision::Entity::find_by_id((
            "workflow-oversized-design-locator".to_owned(),
            1,
        ))
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
        let mut document: ManifestDocument =
            serde_json::from_str(&revision.document_json).unwrap();
        document.design.as_mut().unwrap().rel_path =
            "d".repeat(MAX_SIMPLE_SUCCESSOR_LOCATOR_BYTES + 1);
        let mut revision: delegation_workflow_manifest_revision::ActiveModel = revision.into();
        revision.document_json = Set(serde_json::to_string(&document).unwrap());
        revision.update(&db.conn).await.unwrap();

        let result = continue_archived_workflow_in_simple_core(
            &db,
            &EventEmitter::Noop,
            source,
            "oversized-design-request",
        )
        .await
        .unwrap();

        assert!(result.created);
        assert!(!result.bootstrap_prompt.contains("Design:"));
        assert!(result.bootstrap_prompt.len() <= MAX_SIMPLE_SUCCESSOR_BOOTSTRAP_BYTES);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn simple_successor_omits_design_locator_oversized_after_normalization() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace.path().join("docs")).unwrap();
        std::fs::write(workspace.path().join("docs/plan.md"), "# Plan\n").unwrap();
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, workspace.path().to_str().unwrap()).await;
        let source = seed_conversation(&db, folder, AgentType::Codex).await;
        seed_archived_workflow(
            &db,
            source,
            "workflow-normalized-oversized-design-locator",
            "docs/plan.md",
            Some("docs/design.md"),
            2,
            CompletionProtocolMode::V2Enforce,
        )
        .await;
        let revision = delegation_workflow_manifest_revision::Entity::find_by_id((
            "workflow-normalized-oversized-design-locator".to_owned(),
            1,
        ))
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
        let mut document: ManifestDocument =
            serde_json::from_str(&revision.document_json).unwrap();
        document.design.as_mut().unwrap().rel_path =
            locator_that_expands_past_successor_bound();
        let mut revision: delegation_workflow_manifest_revision::ActiveModel = revision.into();
        revision.document_json = Set(serde_json::to_string(&document).unwrap());
        revision.update(&db.conn).await.unwrap();

        let result = continue_archived_workflow_in_simple_core(
            &db,
            &EventEmitter::Noop,
            source,
            "normalized-oversized-design-request",
        )
        .await
        .unwrap();

        assert!(result.created);
        assert!(!result.bootstrap_prompt.contains("Design:"));
        assert!(result.bootstrap_prompt.len() <= MAX_SIMPLE_SUCCESSOR_BOOTSTRAP_BYTES);
    }

    #[tokio::test]
    async fn simple_successor_omits_malformed_design_locator_from_bootstrap() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace.path().join("docs")).unwrap();
        std::fs::write(workspace.path().join("docs/plan.md"), "# Plan\n").unwrap();
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, workspace.path().to_str().unwrap()).await;
        let source = seed_conversation(&db, folder, AgentType::Codex).await;
        seed_archived_workflow(
            &db,
            source,
            "workflow-malformed-design-locator",
            "docs/plan.md",
            Some("docs/design.md"),
            2,
            CompletionProtocolMode::V2Enforce,
        )
        .await;
        let revision = delegation_workflow_manifest_revision::Entity::find_by_id((
            "workflow-malformed-design-locator".to_owned(),
            1,
        ))
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
        let mut document: ManifestDocument =
            serde_json::from_str(&revision.document_json).unwrap();
        document.design.as_mut().unwrap().rel_path = "../outside-design.md".into();
        let mut revision: delegation_workflow_manifest_revision::ActiveModel = revision.into();
        revision.document_json = Set(serde_json::to_string(&document).unwrap());
        revision.update(&db.conn).await.unwrap();

        let result = continue_archived_workflow_in_simple_core(
            &db,
            &EventEmitter::Noop,
            source,
            "malformed-design-request",
        )
        .await
        .unwrap();

        assert!(result.created);
        assert!(!result.bootstrap_prompt.contains("Design:"));
        assert!(!result.bootstrap_prompt.contains("outside-design.md"));
        assert!(result.bootstrap_prompt.len() <= MAX_SIMPLE_SUCCESSOR_BOOTSTRAP_BYTES);
    }

    #[tokio::test]
    async fn simple_successor_snapshot_race_converges_after_forced_empty_link_reads() {
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
        // Each disk fixture handle has a one-connection pool. Use a second
        // handle so both read transactions can reach the race barrier.
        let competing_db = fresh_disk_db(database.path()).await;
        let control = std::sync::Arc::new(SimpleSuccessorTestControl::snapshot_race(2));

        let (first, second) = tokio::join!(
            continue_archived_workflow_in_simple_controlled(
                &db,
                source,
                "concurrent-request-a",
                control.clone()
            ),
            continue_archived_workflow_in_simple_controlled(
                &competing_db,
                source,
                "concurrent-request-b",
                control.clone()
            )
        );
        let first = first.unwrap();
        let second = second.unwrap();
        assert_eq!(control.empty_link_waits(), 2);
        assert!(control.retries() >= 1);
        assert_eq!(first.successor_conversation_id, second.successor_conversation_id);
        assert_eq!(usize::from(first.created) + usize::from(second.created), 1);
        assert!(first.created ^ second.created);
        assert_eq!(live_conversation_count(&db).await, conversations_before + 1);
        assert_eq!(descriptor_count(&db).await, 1);
    }

    #[tokio::test]
    async fn simple_successor_failure_after_registration_rolls_back_everything() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace.path().join("docs")).unwrap();
        std::fs::write(workspace.path().join("docs/plan.md"), "# Plan\n").unwrap();
        let db = fresh_in_memory_db().await;
        let _suite = title_key::test_hooks::SuiteGuard::enter();
        enable_title_api_for_test(&db.conn).await;
        let folder = seed_folder(&db, workspace.path().to_str().unwrap()).await;
        let source = seed_conversation(&db, folder, AgentType::Codex).await;
        let source_row = conversation::Entity::find_by_id(source)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut source_row: conversation::ActiveModel = source_row.into();
        source_row.title = Set(Some("Rollback source".into()));
        source_row.update(&db.conn).await.unwrap();
        seed_archived_workflow(
            &db,
            source,
            "workflow-successor-rollback",
            "docs/plan.md",
            None,
            2,
            CompletionProtocolMode::V2Enforce,
        )
        .await;
        let source = load_archived_source(&db, source).await.unwrap();
        let conversations_before = live_conversation_count(&db).await;
        let title_jobs_before = auto_title_job::Entity::find().count(&db.conn).await.unwrap();
        let descriptors_before = descriptor_count(&db).await;
        let source_links_before = simple_workflow::Entity::find()
            .filter(simple_workflow::Column::SourceWorkflowId.eq(source.workflow_id.clone()))
            .count(&db.conn)
            .await
            .unwrap();
        let header_before = delegation_workflow::Entity::find_by_id(&source.workflow_id)
            .one(&db.conn)
            .await
            .unwrap();
        let revision_before = delegation_workflow_manifest_revision::Entity::find_by_id((
            source.workflow_id.clone(),
            1,
        ))
        .one(&db.conn)
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
        let control = std::sync::Arc::new(SimpleSuccessorTestControl::fail_after_registration());

        let error = create_or_load_successor_controlled(&db, &source, control.clone())
            .await
            .unwrap_err();

        assert_eq!(error.code, AppErrorCode::DatabaseError);
        assert_eq!(control.rollbacks(), 1);
        assert!(
            control.auto_title_job_seen(),
            "candidate auto-title enrollment must be visible inside the transaction"
        );
        assert!(
            control.descriptor_link_seen(),
            "candidate descriptor and source link must be visible inside the transaction"
        );
        assert_eq!(live_conversation_count(&db).await, conversations_before);
        assert_eq!(descriptor_count(&db).await, descriptors_before);
        assert_eq!(
            simple_workflow::Entity::find()
                .filter(simple_workflow::Column::SourceWorkflowId.eq(source.workflow_id.clone()))
                .count(&db.conn)
                .await
                .unwrap(),
            source_links_before
        );
        assert_eq!(
            auto_title_job::Entity::find().count(&db.conn).await.unwrap(),
            title_jobs_before
        );
        assert!(conversation::Entity::find()
            .filter(conversation::Column::Title.eq("Rollback source (Simple)"))
            .one(&db.conn)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            delegation_workflow::Entity::find_by_id(&source.workflow_id)
                .one(&db.conn)
                .await
                .unwrap(),
            header_before
        );
        assert_eq!(
            delegation_workflow_manifest_revision::Entity::find_by_id((
                source.workflow_id.clone(),
                1,
            ))
            .one(&db.conn)
            .await
            .unwrap(),
            revision_before
        );
        assert_eq!(
            delegation_task_run::Entity::find()
                .count(&db.conn)
                .await
                .unwrap(),
            runs_before
        );
        assert_eq!(
            delegation_workflow_run_binding::Entity::find()
                .count(&db.conn)
                .await
                .unwrap(),
            bindings_before
        );
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
            "recreate-request-a",
        )
        .await
        .unwrap();
        assert!(second.created);
        assert_ne!(second.successor_conversation_id, first.successor_conversation_id);
    }

    #[test]
    fn route_override_type_used_by_successor_stays_wire_stable() {
        assert_eq!(
            serde_json::to_value(DelegationRoutePolicy::Codeg).unwrap(),
            "codeg"
        );
    }
}
