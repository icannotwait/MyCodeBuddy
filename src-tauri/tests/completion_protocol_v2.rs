use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum_test::TestServer;
use chrono::Utc;
use codeg_lib::acp::delegation::broker::{
    is_completion_format_repair_prompt, pre_read_completion_reports_for_test,
    ConversationDepthLookup, DelegationBroker, DelegationConfig, StatusWait,
};
use codeg_lib::acp::delegation::companion::{
    dispatch_line, CompanionContext, CompanionFeatures, InflightCalls, LineAction,
};
use codeg_lib::acp::delegation::event_emitter::mock::MockEventEmitter;
use codeg_lib::acp::delegation::event_emitter::{
    CompletionOutboxDispatcher, CompletionRootWakeQueue, DelegationEventEmitter,
};
use codeg_lib::acp::delegation::lease::CompanionLeaseRegistry;
use codeg_lib::acp::delegation::listener::{
    DelegationListener, ParentSessionLookup, TokenEntry, TokenRegistry,
};
use codeg_lib::acp::delegation::meta_writer::{DelegationMetaWriter, NoopMetaWriter};
use codeg_lib::acp::delegation::metrics::{
    CompletionContinuationReason, CompletionFinalMetricState, DelegationMetrics,
};
use codeg_lib::acp::delegation::run_store::{ReservingRunInsert, RunStore};
use codeg_lib::acp::delegation::spawner::{
    accepted, mock::MockSpawner, ConnectionSpawner, SpawnerError,
};
use codeg_lib::acp::delegation::store::{
    DbDelegationTaskStore, DelegationTaskStore, TaskStoreError,
};
use codeg_lib::acp::delegation::transport::{
    client_cancel_task_round_trip, client_get_workflow_state_round_trip, client_status_round_trip,
    BrokerCancelTaskRequest, BrokerGetWorkflowStateRequest, BrokerStatusRequest,
    CancelDelegationReason, CompanionRole,
};
use codeg_lib::acp::delegation::types::{
    ContinueDelegationRequest, DelegationError, DelegationOutcome, DelegationRequest,
    DelegationSuccess, TaskStatus,
};
use codeg_lib::acp::delegation::workflow::{
    accept_complete_work_txn, build_work_unit_key,
    guard_current_final_delivery_core as production_guard_current_final_delivery,
    guard_final_delivery_core as production_guard_final_delivery,
    guard_task_final_delivery_core as production_guard_task_final_delivery,
    load_completion_protocol_for_conversation, load_historical_workflow_context,
    materialize_terminal_completion_txn, project_workflow_graph_core,
    publish_workflow_manifest_core, publish_workflow_manifest_fixture,
    recover_workflow_core as production_recover_workflow, resolve_completion_decision_txn,
    settle_workflow_gate_v2_core as production_settle_workflow_gate_v2,
    with_historical_workflow_fixture_mutations, CompleteWorkRequest, CompletionCardV2,
    CompletionDecisionResolvedPayloadV1, CompletionIntentSource, CompletionOutcome,
    CompletionProtocolConfigurationRemoved, CompletionRole, DocumentGateKind, DocumentRef,
    FinalDeliveryGuardRequest, FinalDeliveryGuardResult, ManifestDocument, ManifestGate,
    ManifestNode, ManifestNodeKind, ManifestNodeRole, ManifestPhase, ManifestWorkflowState,
    PlanReviewChangeV2, PlanReviewNextAction, PublishWorkflowRequest, RecoverWorkflowRequest,
    RecoverWorkflowResult, ResolutionMode, SettleResult, SettleWorkflowV2Request,
    TerminalCompletionInput, WorkUnitKeyParts, WorkflowStoreError,
    COMPLETION_DECISION_RESOLVED_EVENT, CURRENT_COMPLETION_PROTOCOL_VERSION,
    MANIFEST_SCHEMA_VERSION, PHASE_DESIGN, PHASE_FINAL, PHASE_PLAN,
    WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY,
};
use codeg_lib::acp::error::AcpError;
use codeg_lib::acp::manager::ConnectionManager;
use codeg_lib::acp::question::{QuestionSpec, RegisteredQuestion, SessionQuestionAccess};
use codeg_lib::acp::types::DelegationResultSummary;
use codeg_lib::acp::types::PromptInputBlock;
use codeg_lib::app_state::AppState;
use codeg_lib::db::entities::delegation_workflow_gate_settlement::GateSettlementOutcome;
use codeg_lib::db::entities::{
    auto_title_job, delegation_attention_request, delegation_completion_tool_intent,
    delegation_task_run, delegation_workflow, delegation_workflow_gate_settlement,
    delegation_workflow_gate_state, delegation_workflow_manifest_revision,
    delegation_workflow_node_binding, delegation_workflow_outbox_event,
    delegation_workflow_restart_context, delegation_workflow_run_binding, recovery_authorization,
};
use codeg_lib::db::test_helpers::{
    complete_historical_completion_protocol_migrations, fresh_in_memory_db,
    historical_completion_protocol_db, historical_completion_protocol_db_before_v2_only,
    seed_conversation, seed_folder, HistoricalWorkflowSeed,
};
use codeg_lib::models::AgentType;
use codeg_lib::web::event_bridge::EventEmitter;
use codeg_lib::web::router::build_router;
use codeg_lib::web::shutdown::ShutdownSignal;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, PaginatorTrait,
    QueryFilter, Set, Statement, TransactionTrait,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

async fn settle_workflow_gate_v2_core(
    db: &codeg_lib::db::AppDatabase,
    emitter: &EventEmitter,
    parent_conversation_id: i32,
    request: SettleWorkflowV2Request,
) -> Result<SettleResult, WorkflowStoreError> {
    with_historical_workflow_fixture_mutations(production_settle_workflow_gate_v2(
        db,
        emitter,
        parent_conversation_id,
        request,
    ))
    .await
}

async fn recover_workflow_core(
    db: &codeg_lib::db::AppDatabase,
    emitter: &EventEmitter,
    parent_conversation_id: i32,
    request: RecoverWorkflowRequest,
) -> Result<RecoverWorkflowResult, WorkflowStoreError> {
    with_historical_workflow_fixture_mutations(production_recover_workflow(
        db,
        emitter,
        parent_conversation_id,
        request,
    ))
    .await
}

async fn guard_final_delivery_core(
    db: &codeg_lib::db::AppDatabase,
    emitter: &EventEmitter,
    request: FinalDeliveryGuardRequest,
) -> Result<FinalDeliveryGuardResult, WorkflowStoreError> {
    with_historical_workflow_fixture_mutations(production_guard_final_delivery(
        db, emitter, request,
    ))
    .await
}

async fn guard_current_final_delivery_core(
    db: &codeg_lib::db::AppDatabase,
    emitter: &EventEmitter,
    parent_conversation_id: i32,
    workflow_id: Option<&str>,
) -> Result<Option<FinalDeliveryGuardResult>, WorkflowStoreError> {
    with_historical_workflow_fixture_mutations(production_guard_current_final_delivery(
        db,
        emitter,
        parent_conversation_id,
        workflow_id,
    ))
    .await
}

async fn guard_task_final_delivery_core(
    db: &codeg_lib::db::AppDatabase,
    emitter: &EventEmitter,
    task_id: &str,
) -> Result<Option<FinalDeliveryGuardResult>, WorkflowStoreError> {
    with_historical_workflow_fixture_mutations(production_guard_task_final_delivery(
        db, emitter, task_id,
    ))
    .await
}

#[derive(Default)]
struct RecordingCompletionRootWake {
    event_ids: tokio::sync::Mutex<Vec<String>>,
}

#[async_trait]
impl CompletionRootWakeQueue for RecordingCompletionRootWake {
    async fn enqueue_completion_resolution(
        &self,
        event: &CompletionDecisionResolvedPayloadV1,
    ) -> Result<(), String> {
        self.event_ids.lock().await.push(event.event_id.clone());
        Ok(())
    }
}

#[test]
fn stable_protocol_error_codes() {
    assert_eq!(CURRENT_COMPLETION_PROTOCOL_VERSION, 2);
    assert_eq!(
        codeg_lib::acp::delegation::workflow::current_completion_protocol_mode(),
        delegation_workflow::CompletionProtocolMode::V2Enforce
    );

    let read_only = WorkflowStoreError::LegacyCompletionProtocolReadOnly;
    let unsupported = WorkflowStoreError::UnsupportedCompletionProtocol {
        version: 2,
        mode: delegation_workflow::CompletionProtocolMode::V2Shadow,
    };
    assert_eq!(read_only.code(), "legacy_completion_protocol_read_only");
    assert_eq!(unsupported.code(), "unsupported_completion_protocol");
    assert!(!read_only.is_retryable());
    assert!(!unsupported.is_retryable());

    let configuration_removed = CompletionProtocolConfigurationRemoved {
        variable: "CODEG_COMPLETION_PROTOCOL_MODE",
    };
    assert_eq!(
        configuration_removed.code(),
        "completion_protocol_configuration_removed"
    );

    let acp_errors = [
        (
            AcpError::from(read_only),
            "legacy_completion_protocol_read_only",
        ),
        (
            AcpError::from(unsupported),
            "unsupported_completion_protocol",
        ),
        (
            AcpError::CompletionInstructionBindingFailed("scope mismatch".into()),
            "completion_instruction_binding_failed",
        ),
        (
            AcpError::from(configuration_removed),
            "completion_protocol_configuration_removed",
        ),
    ];
    for (error, expected) in acp_errors {
        assert_eq!(error.code(), Some(expected));
        assert_eq!(serde_json::to_value(error).unwrap()["code"], expected);
    }
}

#[tokio::test]
async fn fixed_v2_creation_persists_across_agent_profiles_and_revisions() {
    let db = fresh_in_memory_db().await;
    let folder = seed_folder(&db, "/tmp/task-2-fixed-v2-creation").await;

    for (index, (conversation_agent, profile_id)) in [
        (AgentType::Codex, None),
        (AgentType::Grok, Some("review-canary")),
        (AgentType::Gemini, Some("reasoning")),
    ]
    .into_iter()
    .enumerate()
    {
        let parent = seed_conversation(&db, folder, conversation_agent).await;
        let token = format!("task-2-fixed-v2-{index}");
        let mut document = skeleton(&token);
        let author = document.nodes.first_mut().expect("plan author");
        author.profile_id = profile_id.map(str::to_string);
        author.work_unit_key = Some(
            build_work_unit_key(&WorkUnitKeyParts::PlanAuthor {
                rel_plan_path: &document.plan_target_rel_path,
                agent_type: "codex",
                profile_id,
            })
            .unwrap(),
        );

        let published = publish_workflow_manifest_fixture(
            &db,
            &EventEmitter::Noop,
            parent,
            PublishWorkflowRequest {
                document: document.clone(),
            },
        )
        .await
        .unwrap();
        let row = delegation_workflow::Entity::find_by_id(&published.workflow_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.completion_protocol_version, 2);
        assert_eq!(
            row.completion_protocol_mode,
            delegation_workflow::CompletionProtocolMode::V2Enforce
        );

        document.workflow_id = Some(published.workflow_id.clone());
        document.expected_manifest_revision = Some(published.manifest_revision);
        document.nodes[0].title = Some("revised display title".into());
        let revised = publish_workflow_manifest_fixture(
            &db,
            &EventEmitter::Noop,
            parent,
            PublishWorkflowRequest { document },
        )
        .await
        .unwrap();
        assert_eq!(revised.manifest_revision, published.manifest_revision + 1);

        let revised_row = delegation_workflow::Entity::find_by_id(&published.workflow_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(revised_row.completion_protocol_version, 2);
        assert_eq!(
            revised_row.completion_protocol_mode,
            delegation_workflow::CompletionProtocolMode::V2Enforce
        );
    }
}

#[tokio::test]
async fn fixed_v2_creation_rejects_revision_after_header_becomes_historical() {
    let parent = 101;
    let workflow_id = "wf-task-2-fixed-v2-historical-revision";
    let db = historical_completion_protocol_db(&[historical_workflow_seed(
        workflow_id,
        parent,
        1,
        delegation_workflow::CompletionProtocolMode::V1,
        None,
    )])
    .await;
    let mut document = skeleton("task-2-fixed-v2-historical-revision");
    let published = seed_historical_manifest(&db, workflow_id, &document).await;

    document.workflow_id = Some(published.workflow_id.clone());
    document.expected_manifest_revision = Some(published.manifest_revision);
    document.nodes[0].title = Some("must remain read only".into());
    let error = publish_workflow_manifest_core(
        &db,
        &EventEmitter::Noop,
        parent,
        PublishWorkflowRequest { document },
    )
    .await
    .expect_err("historical workflow revision must be rejected");
    assert_eq!(error.code(), "workflow_v2_retired");

    let row = delegation_workflow::Entity::find_by_id(&published.workflow_id)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.active_manifest_revision, 1);
    assert_eq!(row.completion_protocol_version, 1);
}

struct RootDepth;

#[async_trait]
impl ConversationDepthLookup for RootDepth {
    async fn parent_of(&self, _id: i32) -> Result<Option<i32>, DelegationError> {
        Ok(None)
    }
}

struct FixedParent(i32);

#[async_trait]
impl ParentSessionLookup for FixedParent {
    async fn current_conversation_id(&self, _parent_connection_id: &str) -> Option<i32> {
        Some(self.0)
    }
}

struct NoFeedback;

#[async_trait]
impl codeg_lib::acp::feedback::SessionFeedbackAccess for NoFeedback {
    async fn read_pending_feedback(
        &self,
        _parent_connection_id: &str,
    ) -> Vec<codeg_lib::acp::feedback::PendingFeedback> {
        Vec::new()
    }

    async fn commit_feedback_delivered(&self, _parent_connection_id: &str, _ids: Vec<String>) {}
}

struct NoQuestions;

#[async_trait]
impl SessionQuestionAccess for NoQuestions {
    async fn register_question(
        &self,
        _parent_connection_id: &str,
        _questions: Vec<QuestionSpec>,
    ) -> Option<RegisteredQuestion> {
        None
    }

    async fn cancel_question(&self, _parent_connection_id: &str, _question_id: &str) {}

    async fn cancel_questions_by_parent(&self, _parent_connection_id: &str) {}
}

struct NoSessionInfo;

#[async_trait]
impl codeg_lib::acp::session_info::SessionInfoAccess for NoSessionInfo {
    async fn resolve(
        &self,
        session_id: i32,
        _max_messages: u32,
    ) -> codeg_lib::acp::session_info::SessionInfo {
        codeg_lib::acp::session_info::SessionInfo::not_found(session_id)
    }
}

#[cfg(windows)]
fn workflow_socket_path() -> std::path::PathBuf {
    std::path::PathBuf::from(format!(
        r"\\.\pipe\codeg-task-18-final-{}",
        uuid::Uuid::new_v4()
    ))
}

#[cfg(unix)]
fn workflow_socket_path() -> std::path::PathBuf {
    // AF_UNIX sun_path is typically 104–108 bytes. macOS TMPDIR is often long
    // enough that `temp_dir()/codeg-task-18-final-{uuid}.sock` exceeds SUN_LEN
    // and bind/connect fail with InvalidInput. Keep a short absolute path under
    // /tmp (same approach as other delegation e2e fixtures).
    std::path::PathBuf::from(format!(
        "/tmp/cg18-{}.sock",
        &uuid::Uuid::new_v4().simple().to_string()[..16]
    ))
}

async fn wait_for_workflow_listener(socket_path: &Path, token: &str, workflow_id: &str) -> Value {
    let request = BrokerGetWorkflowStateRequest {
        token: token.into(),
        workflow_id: Some(workflow_id.into()),
    };
    let mut last_error = None;
    for _ in 0..50 {
        match client_get_workflow_state_round_trip(socket_path.to_string_lossy().as_ref(), &request)
            .await
        {
            Ok(response) => return response.outcome,
            Err(error) => {
                last_error = Some(error);
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
    panic!("workflow listener did not start: {last_error:?}");
}

fn is_listener_not_ready(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound
            | std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::ConnectionReset
    )
}

async fn client_status_when_ready(
    socket_path: &str,
    request: &BrokerStatusRequest,
) -> std::io::Result<codeg_lib::acp::delegation::transport::BrokerResponse> {
    let mut last_error = None;
    for _ in 0..50 {
        match client_status_round_trip(socket_path, request).await {
            Ok(response) => return Ok(response),
            Err(error) if is_listener_not_ready(&error) => {
                last_error = Some(error);
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "listener never accepted status",
        )
    }))
}

async fn client_cancel_task_when_ready(
    socket_path: &str,
    request: &BrokerCancelTaskRequest,
) -> std::io::Result<codeg_lib::acp::delegation::transport::BrokerResponse> {
    let mut last_error = None;
    for _ in 0..50 {
        match client_cancel_task_round_trip(socket_path, request).await {
            Ok(response) => return Ok(response),
            Err(error) if is_listener_not_ready(&error) => {
                last_error = Some(error);
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "listener never accepted cancel",
        )
    }))
}

async fn companion_tool_names(context: &CompanionContext) -> Vec<String> {
    let line = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
    let LineAction::Respond(response) =
        dispatch_line(context, Arc::new(InflightCalls::new()), line).await
    else {
        panic!("tools/list must respond synchronously")
    };
    response.result.expect("tools/list result")["tools"]
        .as_array()
        .expect("tools/list array")
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_string))
        .collect()
}

async fn call_companion_tool(
    context: &CompanionContext,
    id: u64,
    name: &str,
    arguments: Value,
    tool_use_id: Option<&str>,
) -> Value {
    let mut params = json!({ "name": name, "arguments": arguments });
    if let Some(tool_use_id) = tool_use_id {
        params["_meta"] = json!({ "tool_use_id": tool_use_id });
    }
    let line = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": params,
    })
    .to_string();
    let LineAction::Spawn(call) =
        dispatch_line(context, Arc::new(InflightCalls::new()), &line).await
    else {
        panic!("{name} must cross the companion transport")
    };
    let response = call.future.await.response.expect("companion tool response");
    if let Some(error) = response.error {
        panic!("{name} failed: {}", error.message);
    }
    response.result.expect("companion tool result")
}

fn skeleton(token: &str) -> ManifestDocument {
    let plan_path = "docs/superpowers/plans/restarted-plan.md";
    ManifestDocument {
        schema_version: MANIFEST_SCHEMA_VERSION,
        workflow_kind: WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY.into(),
        plan_target_rel_path: plan_path.into(),
        risk_policy_version: "b2d_task_risk_v1".into(),
        workflow_id: None,
        expected_manifest_revision: None,
        publication_token: token.into(),
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
        nodes: vec![ManifestNode {
            id: "plan-author".into(),
            kind: ManifestNodeKind::WorkUnit,
            phase_id: Some(PHASE_PLAN.into()),
            role: Some(ManifestNodeRole::Author),
            agent_type: Some("codex".into()),
            profile_id: None,
            task_index: None,
            work_unit_key: Some(
                build_work_unit_key(&WorkUnitKeyParts::PlanAuthor {
                    rel_plan_path: plan_path,
                    agent_type: "codex",
                    profile_id: None,
                })
                .unwrap(),
            ),
            deps: Vec::new(),
            required: Some(true),
            node_outcome: None,
            title: None,
        }],
        edges: Vec::new(),
        gates: Vec::new(),
        task_policies: Vec::new(),
    }
}

struct HistoricalPublication {
    workflow_id: String,
    manifest_revision: u64,
    graph_revision: u64,
}

fn historical_workflow_seed(
    workflow_id: impl Into<String>,
    parent_conversation_id: i32,
    version: i64,
    mode: delegation_workflow::CompletionProtocolMode,
    legacy_source_workflow_id: Option<String>,
) -> HistoricalWorkflowSeed {
    HistoricalWorkflowSeed {
        workflow_id: workflow_id.into(),
        parent_conversation_id,
        version,
        mode,
        legacy_source_workflow_id,
    }
}

async fn seed_historical_manifest(
    db: &codeg_lib::db::AppDatabase,
    workflow_id: &str,
    document: &ManifestDocument,
) -> HistoricalPublication {
    let mut stored_document = document.clone();
    stored_document.workflow_id = Some(workflow_id.to_owned());
    stored_document.expected_manifest_revision = None;
    let document_json = serde_json::to_string(&stored_document).unwrap();
    let now = chrono::Utc::now();
    db.conn
        .execute(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "UPDATE delegation_workflows SET publication_token = ? WHERE workflow_id = ?",
            vec![
                stored_document.publication_token.clone().into(),
                workflow_id.into(),
            ],
        ))
        .await
        .unwrap();
    let manifest_state = serde_json::to_value(stored_document.workflow_state)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned();
    delegation_workflow_manifest_revision::ActiveModel {
        workflow_id: Set(workflow_id.to_owned()),
        manifest_revision: Set(1),
        manifest_state: Set(manifest_state),
        document_digest: Set(format!(
            "sha256:{:x}",
            Sha256::digest(document_json.as_bytes())
        )),
        document_json: Set(document_json),
        revision_kind: Set(Some("initial".into())),
        source_manifest_revision: Set(None),
        recovery_authorization_id: Set(None),
        transition_reason_code: Set(None),
        consumer_correlation_id: Set(None),
        graph_revision: Set(Some(1)),
        recovery_source_state_fingerprint: Set(None),
        recovery_risk_class: Set(None),
        created_at: Set(now),
    }
    .insert(&db.conn)
    .await
    .unwrap();

    for node in &stored_document.nodes {
        let (Some(work_unit_key), Some(role), Some(agent_type), Some(phase_id)) = (
            node.work_unit_key.as_ref(),
            node.role,
            node.agent_type.as_ref(),
            node.phase_id.as_ref(),
        ) else {
            continue;
        };
        let role = serde_json::to_value(role)
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned();
        delegation_workflow_node_binding::ActiveModel {
            workflow_id: Set(workflow_id.to_owned()),
            node_id: Set(node.id.clone()),
            work_unit_key: Set(work_unit_key.clone()),
            role: Set(role),
            agent_type: Set(agent_type.clone()),
            profile_id: Set(node.profile_id.clone()),
            phase_id: Set(phase_id.clone()),
            task_index: Set(node.task_index.map(i64::from)),
            introduced_revision: Set(1),
            retired_revision: Set(None),
            is_observed: Set(false),
            retained_observed: Set(false),
            cohort_frozen: Set(false),
            node_outcome: Set(node
                .node_outcome
                .map(|_| delegation_workflow_node_binding::NodeOutcome::Canceled)),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&db.conn)
        .await
        .unwrap();
    }

    HistoricalPublication {
        workflow_id: workflow_id.to_owned(),
        manifest_revision: 1,
        graph_revision: 1,
    }
}

async fn mutation_snapshot(
    db: &codeg_lib::db::AppDatabase,
    parent: i32,
    workflow_id: &str,
) -> String {
    let workflow = delegation_workflow::Entity::find_by_id(workflow_id)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    let conversation = codeg_lib::db::entities::conversation::Entity::find_by_id(parent)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    let revisions = delegation_workflow_manifest_revision::Entity::find()
        .filter(delegation_workflow_manifest_revision::Column::WorkflowId.eq(workflow_id))
        .count(&db.conn)
        .await
        .unwrap();
    let settlements = delegation_workflow_gate_settlement::Entity::find()
        .filter(delegation_workflow_gate_settlement::Column::WorkflowId.eq(workflow_id))
        .count(&db.conn)
        .await
        .unwrap();
    let attentions = delegation_attention_request::Entity::find()
        .filter(delegation_attention_request::Column::ParentConversationId.eq(parent))
        .count(&db.conn)
        .await
        .unwrap();
    let bindings = delegation_workflow_run_binding::Entity::find()
        .filter(delegation_workflow_run_binding::Column::WorkflowId.eq(workflow_id))
        .count(&db.conn)
        .await
        .unwrap();
    let intents = delegation_completion_tool_intent::Entity::find()
        .count(&db.conn)
        .await
        .unwrap();
    let authorizations = recovery_authorization::Entity::find()
        .filter(recovery_authorization::Column::ParentConversationId.eq(parent))
        .count(&db.conn)
        .await
        .unwrap();
    let successors = delegation_workflow::Entity::find()
        .filter(delegation_workflow::Column::LegacySourceWorkflowId.eq(workflow_id))
        .count(&db.conn)
        .await
        .unwrap();
    format!(
        "{workflow:?}|{conversation:?}|revisions={revisions}|settlements={settlements}|attentions={attentions}|bindings={bindings}|intents={intents}|authorizations={authorizations}|successors={successors}"
    )
}

async fn seed_final_guard_binding(
    db: &codeg_lib::db::AppDatabase,
    parent: i32,
    child: i32,
    workflow_id: &str,
    task_id: &str,
) {
    let runs = RunStore::new(Arc::new(codeg_lib::db::AppDatabase {
        conn: db.conn.clone(),
    }));
    runs.insert_reserving(ReservingRunInsert {
        orchestration_binding: None,
        task_id: task_id.into(),
        root_task_id: task_id.into(),
        previous_task_id: None,
        generation: 1,
        parent_conversation_id: parent,
        parent_tool_use_id: Some(format!("tool-{task_id}")),
        child_conversation_id: child,
        agent_type: "codex".into(),
        profile_id: None,
        workspace_path: Some("/tmp/task-4-final-guard".into()),
        route_fingerprint: Some(format!("route-{task_id}")),
        launch_snapshot_version: Some("v1".into()),
        mode_id: None,
        config_values_json: Some("{}".into()),
        task_preview: Some("Task 4 Final delivery guard".into()),
        request_fingerprint: Some(format!("fingerprint-{task_id}")),
        admission_class: delegation_task_run::AdmissionClass::NormalRevision,
        lineage_root_task_id: task_id.into(),
        work_unit_key: None,
        history_only: false,
        replaced_task_id: None,
        replacement_reason: None,
        started_at: Some(chrono::Utc::now()),
    })
    .await
    .unwrap();
    let now = chrono::Utc::now();
    delegation_workflow_run_binding::ActiveModel {
        task_id: Set(task_id.into()),
        workflow_id: Set(workflow_id.into()),
        node_id: Set("plan-author".into()),
        gate_id: Set(None),
        gate_cycle: Set(None),
        manifest_revision: Set(1),
        content_fingerprint: Set(None),
        evidence_scope_digest: Set(None),
        gate_lineage: Set(None),
        review_round: Set(None),
        instruction_block_digest: Set(None),
        material_selector_digest: Set(None),
        subject_material_digest: Set(None),
        requirements_identity: Set(None),
        task_specification_identity: Set(None),
        final_findings_identity: Set(None),
        producer_baseline_head: Set(None),
        artifact_digest: Set(None),
        reviewed_task_id: Set(None),
        reviewed_implementer_generation: Set(None),
        lineage_ordinal: Set(1),
        summary_validated: Set(false),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&db.conn)
    .await
    .unwrap();
}

async fn seed_conversation_workflow_association(
    db: &codeg_lib::db::AppDatabase,
    parent: i32,
    child: i32,
    task_id: &str,
    generation: i64,
    created_at: chrono::DateTime<chrono::Utc>,
    workflow_id: Option<&str>,
) {
    let runs = RunStore::new(Arc::new(codeg_lib::db::AppDatabase {
        conn: db.conn.clone(),
    }));
    runs.insert_reserving(ReservingRunInsert {
        orchestration_binding: None,
        task_id: task_id.into(),
        root_task_id: format!("root-{child}"),
        previous_task_id: None,
        generation,
        parent_conversation_id: parent,
        parent_tool_use_id: Some(format!("tool-{task_id}")),
        child_conversation_id: child,
        agent_type: "codex".into(),
        profile_id: None,
        workspace_path: Some("/tmp/task-4-root-associations".into()),
        route_fingerprint: Some(format!("route-{task_id}")),
        launch_snapshot_version: Some("v1".into()),
        mode_id: None,
        config_values_json: Some("{}".into()),
        task_preview: Some("Task 4 root association fence".into()),
        request_fingerprint: Some(format!("fingerprint-{task_id}")),
        admission_class: delegation_task_run::AdmissionClass::NormalRevision,
        lineage_root_task_id: format!("root-{child}"),
        work_unit_key: None,
        history_only: false,
        replaced_task_id: None,
        replacement_reason: None,
        started_at: Some(created_at),
    })
    .await
    .unwrap();
    let run = delegation_task_run::Entity::find_by_id(task_id)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    let mut run: delegation_task_run::ActiveModel = run.into();
    run.status = Set(delegation_task_run::DelegationRunStatus::Completed);
    run.finished_at = Set(Some(created_at));
    run.created_at = Set(created_at);
    run.updated_at = Set(created_at);
    run.update(&db.conn).await.unwrap();

    let Some(workflow_id) = workflow_id else {
        return;
    };
    let now = chrono::Utc::now();
    delegation_workflow_run_binding::ActiveModel {
        task_id: Set(task_id.into()),
        workflow_id: Set(workflow_id.into()),
        node_id: Set("plan-author".into()),
        gate_id: Set(None),
        gate_cycle: Set(None),
        manifest_revision: Set(1),
        content_fingerprint: Set(None),
        evidence_scope_digest: Set(None),
        gate_lineage: Set(None),
        review_round: Set(None),
        instruction_block_digest: Set(None),
        material_selector_digest: Set(None),
        subject_material_digest: Set(None),
        requirements_identity: Set(None),
        task_specification_identity: Set(None),
        final_findings_identity: Set(None),
        producer_baseline_head: Set(None),
        artifact_digest: Set(None),
        reviewed_task_id: Set(None),
        reviewed_implementer_generation: Set(None),
        lineage_ordinal: Set(generation),
        summary_validated: Set(false),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&db.conn)
    .await
    .unwrap();
}

#[tokio::test]
async fn historical_protocol_mutation_matrix() {
    use delegation_workflow::CompletionProtocolMode::{V2Enforce, V2Shadow, V1};

    for (index, version, mode, expected_code) in [
        (0, 1, V1, "legacy_completion_protocol_read_only"),
        (1, 1, V2Shadow, "legacy_completion_protocol_read_only"),
        (2, 1, V2Enforce, "legacy_completion_protocol_read_only"),
        (3, 2, V1, "unsupported_completion_protocol"),
        (4, 2, V2Shadow, "unsupported_completion_protocol"),
    ] {
        let workspace = tempfile::tempdir().unwrap();
        let parent = 101;
        let workflow_id = format!("wf-task-4-mutation-matrix-{index}");
        let db = historical_completion_protocol_db(&[historical_workflow_seed(
            &workflow_id,
            parent,
            version,
            mode,
            None,
        )])
        .await;
        let child = seed_conversation(&db, 1, AgentType::Codex).await;
        let mut document = skeleton(&format!("task-4-mutation-matrix-{index}"));
        let published = seed_historical_manifest(&db, &workflow_id, &document).await;
        let task_id = format!("task-4-final-guard-{index}");
        seed_final_guard_binding(&db, parent, child, &published.workflow_id, &task_id).await;
        let before = mutation_snapshot(&db, parent, &published.workflow_id).await;

        document.workflow_id = Some(published.workflow_id.clone());
        document.expected_manifest_revision = Some(published.manifest_revision);
        document.nodes[0].title = Some("must not publish".into());
        let publish_error = publish_workflow_manifest_core(
            &db,
            &EventEmitter::Noop,
            parent,
            PublishWorkflowRequest { document },
        )
        .await
        .expect_err("rejected protocol pair must not publish");
        assert_eq!(publish_error.code(), "workflow_v2_retired");

        for (gate_id, expected_review_round, expected_outcome) in [
            ("design", Some(1), Some(GateSettlementOutcome::Approved)),
            ("plan", Some(1), Some(GateSettlementOutcome::Approved)),
        ] {
            let error = settle_workflow_gate_v2_core(
                &db,
                &EventEmitter::Noop,
                parent,
                SettleWorkflowV2Request {
                    workflow_id: published.workflow_id.clone(),
                    gate_id: gate_id.into(),
                    expected_graph_revision: published.graph_revision,
                    expected_review_round,
                    expected_outcome,
                    summary: "must not settle".into(),
                    recovery_authorization_id: None,
                },
            )
            .await
            .expect_err("rejected protocol pair must not settle");
            assert_eq!(error.code(), expected_code);
        }

        let recovery_error = recover_workflow_core(
            &db,
            &EventEmitter::Noop,
            parent,
            RecoverWorkflowRequest {
                workflow_id: published.workflow_id.clone(),
                recovery_authorization_id: "must-not-consume".into(),
                expected_manifest_revision: published.manifest_revision,
                correlation_id: format!("task-4-recover-{index}"),
            },
        )
        .await
        .expect_err("rejected protocol pair must not recover");
        assert_eq!(recovery_error.code(), expected_code);

        let direct_final_error = guard_final_delivery_core(
            &db,
            &EventEmitter::Noop,
            FinalDeliveryGuardRequest {
                workflow_id: published.workflow_id.clone(),
                gate_id: PHASE_FINAL.into(),
                workspace_path: workspace.path().to_path_buf(),
                final_reviewer_task_id: task_id.clone(),
            },
        )
        .await
        .expect_err("rejected protocol pair must not reach direct Final delivery");
        assert_eq!(direct_final_error.code(), expected_code);

        let current_final_error = guard_current_final_delivery_core(
            &db,
            &EventEmitter::Noop,
            parent,
            Some(&published.workflow_id),
        )
        .await
        .expect_err("rejected protocol pair must not reach current Final delivery");
        assert_eq!(current_final_error.code(), expected_code);

        let task_final_error = guard_task_final_delivery_core(&db, &EventEmitter::Noop, &task_id)
            .await
            .expect_err("rejected protocol pair must not reach task Final delivery");
        assert_eq!(task_final_error.code(), expected_code);

        assert_eq!(
            mutation_snapshot(&db, parent, &published.workflow_id).await,
            before,
            "rejected pair version={version} must preserve every tracked side effect"
        );
    }
}

#[tokio::test]
async fn workflow_admission_requires_v2() {
    use delegation_workflow::CompletionProtocolMode::{V2Enforce, V2Shadow, V1};

    for (index, version, mode, expected_code) in [
        (0, 1, V1, "legacy_completion_protocol_read_only"),
        (1, 1, V2Shadow, "legacy_completion_protocol_read_only"),
        (2, 1, V2Enforce, "legacy_completion_protocol_read_only"),
        (3, 2, V1, "unsupported_completion_protocol"),
        (4, 2, V2Shadow, "unsupported_completion_protocol"),
    ] {
        let workspace = tempfile::tempdir().unwrap();
        let parent = 101;
        let workflow_id = format!("wf-task-5-admission-{index}");
        let db = historical_completion_protocol_db(&[historical_workflow_seed(
            &workflow_id,
            parent,
            version,
            mode,
            None,
        )])
        .await;
        let child = seed_conversation(&db, 1, AgentType::Codex).await;
        let document = skeleton(&format!("task-5-admission-{index}"));
        let work_unit_key = document.nodes[0].work_unit_key.clone().unwrap();
        let _published = seed_historical_manifest(&db, &workflow_id, &document).await;

        let task_id = format!("task-5-admission-run-{index}");
        let runs = RunStore::new(Arc::new(codeg_lib::db::AppDatabase {
            conn: db.conn.clone(),
        }));
        let error = runs
            .admit_gen1_reserving(ReservingRunInsert {
                orchestration_binding: None,
                task_id: task_id.clone(),
                root_task_id: task_id.clone(),
                previous_task_id: None,
                generation: 1,
                parent_conversation_id: parent,
                parent_tool_use_id: Some(format!("tool-{task_id}")),
                child_conversation_id: child,
                agent_type: "codex".into(),
                profile_id: None,
                workspace_path: Some(workspace.path().to_string_lossy().into_owned()),
                route_fingerprint: Some(format!("route-{task_id}")),
                launch_snapshot_version: Some("v1".into()),
                mode_id: None,
                config_values_json: Some("{}".into()),
                task_preview: Some("Task 5 v2 admission fence".into()),
                request_fingerprint: Some(format!("fingerprint-{task_id}")),
                admission_class: delegation_task_run::AdmissionClass::NormalRevision,
                lineage_root_task_id: task_id.clone(),
                work_unit_key: Some(work_unit_key),
                history_only: false,
                replaced_task_id: None,
                replacement_reason: None,
                started_at: Some(chrono::Utc::now()),
            })
            .await
            .expect_err("non-v2 workflow admission must fail closed");
        assert_eq!(error.workflow_admission_code(), Some(expected_code));
        assert!(
            delegation_task_run::Entity::find_by_id(&task_id)
                .one(&db.conn)
                .await
                .unwrap()
                .is_none(),
            "rejected admission must roll back the reserving run"
        );
        assert!(
            delegation_workflow_run_binding::Entity::find_by_id(&task_id)
                .one(&db.conn)
                .await
                .unwrap()
                .is_none(),
            "rejected admission must not create a workflow run binding"
        );
    }
}

#[tokio::test]
async fn terminal_protocol_failure_is_typed() {
    use delegation_workflow::CompletionProtocolMode::{V2Enforce, V2Shadow, V1};

    for (index, version, mode, expected_code) in [
        (0, 1, V1, "legacy_completion_protocol_read_only"),
        (1, 1, V2Shadow, "legacy_completion_protocol_read_only"),
        (2, 1, V2Enforce, "legacy_completion_protocol_read_only"),
        (3, 2, V1, "unsupported_completion_protocol"),
        (4, 2, V2Shadow, "unsupported_completion_protocol"),
    ] {
        let parent = 101;
        let workflow_id = format!("wf-task-5-terminal-{index}");
        let db = historical_completion_protocol_db(&[historical_workflow_seed(
            &workflow_id,
            parent,
            version,
            mode,
            None,
        )])
        .await;
        let child = seed_conversation(&db, 1, AgentType::Codex).await;
        let published = seed_historical_manifest(
            &db,
            &workflow_id,
            &skeleton(&format!("task-5-terminal-{index}")),
        )
        .await;
        let task_id = format!("task-5-terminal-run-{index}");
        seed_conversation_workflow_association(
            &db,
            parent,
            child,
            &task_id,
            1,
            chrono::Utc::now(),
            Some(&published.workflow_id),
        )
        .await;

        let runs = RunStore::new(Arc::new(codeg_lib::db::AppDatabase {
            conn: db.conn.clone(),
        }));
        let error = runs
            .terminal_completion_protocol(&task_id)
            .await
            .expect_err("non-v2 terminal protocol must be a typed rejection");
        assert!(matches!(error, TaskStoreError::WorkflowAdmission { .. }));
        assert_eq!(error.workflow_admission_code(), Some(expected_code));
    }

    for corruption in ["dangling", "unknown-version", "corrupt-mode"] {
        let db = if corruption == "dangling" {
            fresh_in_memory_db().await
        } else {
            historical_completion_protocol_db_before_v2_only().await
        };
        let folder = seed_folder(&db, "/tmp/task-5-terminal-corrupt-header").await;
        let parent = seed_conversation(&db, folder, AgentType::Codex).await;
        let child = seed_conversation(&db, folder, AgentType::Codex).await;
        let published = publish_workflow_manifest_fixture(
            &db,
            &EventEmitter::Noop,
            parent,
            PublishWorkflowRequest {
                document: skeleton(&format!("task-5-terminal-{corruption}")),
            },
        )
        .await
        .unwrap();
        let workflow_id = published.workflow_id;
        match corruption {
            "unknown-version" => {
                corrupt_protocol_header(&db, &workflow_id, 99, "v2_enforce").await;
                complete_historical_completion_protocol_migrations(&db).await;
            }
            "corrupt-mode" => {
                corrupt_protocol_header(&db, &workflow_id, 2, "future_mode").await;
                complete_historical_completion_protocol_migrations(&db).await;
            }
            _ => {}
        }
        let task_id = format!("task-5-terminal-{corruption}-run");
        seed_conversation_workflow_association(
            &db,
            parent,
            child,
            &task_id,
            1,
            chrono::Utc::now(),
            Some(&workflow_id),
        )
        .await;

        match corruption {
            "dangling" => {
                db.conn
                    .execute(Statement::from_string(
                        DbBackend::Sqlite,
                        "PRAGMA foreign_keys = OFF".to_string(),
                    ))
                    .await
                    .unwrap();
                db.conn
                    .execute(Statement::from_sql_and_values(
                        DbBackend::Sqlite,
                        "DELETE FROM delegation_workflows WHERE workflow_id = ?",
                        vec![workflow_id.clone().into()],
                    ))
                    .await
                    .unwrap();
                db.conn
                    .execute(Statement::from_string(
                        DbBackend::Sqlite,
                        "PRAGMA foreign_keys = ON".to_string(),
                    ))
                    .await
                    .unwrap();
            }
            "unknown-version" | "corrupt-mode" => {}
            _ => unreachable!(),
        }

        let runs = RunStore::new(Arc::new(codeg_lib::db::AppDatabase {
            conn: db.conn.clone(),
        }));
        let error = runs
            .terminal_completion_protocol(&task_id)
            .await
            .expect_err("corrupt or dangling terminal header must fail closed");
        assert!(matches!(error, TaskStoreError::WorkflowAdmission { .. }));
        assert_eq!(
            error.workflow_admission_code(),
            Some("unsupported_completion_protocol"),
            "{corruption}"
        );
    }
}

#[tokio::test]
async fn historical_protocol_cross_parent_mutations_remain_unauthorized() {
    let owner = 101;
    let workflow_id = "wf-task-4-cross-parent-protocol-fence";
    let db = historical_completion_protocol_db(&[historical_workflow_seed(
        workflow_id,
        owner,
        1,
        delegation_workflow::CompletionProtocolMode::V1,
        None,
    )])
    .await;
    let foreign_folder = seed_folder(&db, "/tmp/task-4-foreign-fence").await;
    let foreign = seed_conversation(&db, foreign_folder, AgentType::Codex).await;
    let published = seed_historical_manifest(
        &db,
        workflow_id,
        &skeleton("task-4-cross-parent-protocol-fence"),
    )
    .await;
    let before = mutation_snapshot(&db, owner, &published.workflow_id).await;

    let mut foreign_revision = skeleton("task-4-cross-parent-protocol-fence");
    foreign_revision.workflow_id = Some(published.workflow_id.clone());
    foreign_revision.expected_manifest_revision = Some(published.manifest_revision);
    foreign_revision.nodes[0].title = Some("foreign revision".into());
    let publication_error = publish_workflow_manifest_core(
        &db,
        &EventEmitter::Noop,
        foreign,
        PublishWorkflowRequest {
            document: foreign_revision,
        },
    )
    .await
    .expect_err("cross-parent publication must remain unauthorized");
    assert_eq!(publication_error.code(), "workflow_v2_retired");

    let settle_error = settle_workflow_gate_v2_core(
        &db,
        &EventEmitter::Noop,
        foreign,
        SettleWorkflowV2Request {
            workflow_id: published.workflow_id.clone(),
            gate_id: "design".into(),
            expected_graph_revision: published.graph_revision,
            expected_review_round: Some(1),
            expected_outcome: Some(GateSettlementOutcome::Approved),
            summary: "foreign caller must not learn the protocol".into(),
            recovery_authorization_id: None,
        },
    )
    .await
    .expect_err("cross-parent settlement must remain unauthorized");
    assert_eq!(settle_error.code(), "unauthorized");

    let recovery_error = recover_workflow_core(
        &db,
        &EventEmitter::Noop,
        foreign,
        RecoverWorkflowRequest {
            workflow_id: published.workflow_id.clone(),
            recovery_authorization_id: "foreign-authorization".into(),
            expected_manifest_revision: published.manifest_revision,
            correlation_id: "task-4-cross-parent-recovery".into(),
        },
    )
    .await
    .expect_err("cross-parent recovery must remain unauthorized");
    assert_eq!(recovery_error.code(), "unauthorized");

    let final_error = guard_current_final_delivery_core(
        &db,
        &EventEmitter::Noop,
        foreign,
        Some(&published.workflow_id),
    )
    .await
    .expect_err("cross-parent Final guard must remain unauthorized");
    assert_eq!(final_error.code(), "unauthorized");
    assert_eq!(
        mutation_snapshot(&db, owner, &published.workflow_id).await,
        before
    );
}

async fn corrupt_mutation_snapshot(
    db: &codeg_lib::db::AppDatabase,
    parent: i32,
    workflow_id: &str,
) -> String {
    let header = db
        .conn
        .query_one(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT completion_protocol_version, completion_protocol_mode, active_manifest_revision, graph_revision FROM delegation_workflows WHERE workflow_id = ?",
            vec![workflow_id.into()],
        ))
        .await
        .unwrap()
        .unwrap();
    let version: i64 = header.try_get("", "completion_protocol_version").unwrap();
    let mode: String = header.try_get("", "completion_protocol_mode").unwrap();
    let manifest_revision: i64 = header.try_get("", "active_manifest_revision").unwrap();
    let graph_revision: i64 = header.try_get("", "graph_revision").unwrap();
    let conversation = codeg_lib::db::entities::conversation::Entity::find_by_id(parent)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    let revisions = delegation_workflow_manifest_revision::Entity::find()
        .filter(delegation_workflow_manifest_revision::Column::WorkflowId.eq(workflow_id))
        .count(&db.conn)
        .await
        .unwrap();
    let settlements = delegation_workflow_gate_settlement::Entity::find()
        .filter(delegation_workflow_gate_settlement::Column::WorkflowId.eq(workflow_id))
        .count(&db.conn)
        .await
        .unwrap();
    let attentions = delegation_attention_request::Entity::find()
        .filter(delegation_attention_request::Column::ParentConversationId.eq(parent))
        .count(&db.conn)
        .await
        .unwrap();
    let authorizations = recovery_authorization::Entity::find()
        .filter(recovery_authorization::Column::ParentConversationId.eq(parent))
        .count(&db.conn)
        .await
        .unwrap();
    format!(
        "version={version}|mode={mode}|manifest={manifest_revision}|graph={graph_revision}|{conversation:?}|revisions={revisions}|settlements={settlements}|attentions={attentions}|authorizations={authorizations}"
    )
}

async fn corrupt_protocol_header(
    db: &codeg_lib::db::AppDatabase,
    workflow_id: &str,
    version: i64,
    mode: &str,
) {
    db.conn
        .execute_unprepared("PRAGMA ignore_check_constraints = ON")
        .await
        .unwrap();
    let update = db
        .conn
        .execute(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "UPDATE delegation_workflows SET completion_protocol_version = ?, completion_protocol_mode = ? WHERE workflow_id = ?",
            vec![version.into(), mode.into(), workflow_id.into()],
        ))
        .await;
    db.conn
        .execute_unprepared("PRAGMA ignore_check_constraints = OFF")
        .await
        .unwrap();
    update.unwrap();
}

#[tokio::test]
async fn corrupt_header_nonterminal_fences() {
    for (index, version, mode) in [(0, 99, "v2_enforce"), (1, 2, "corrupt_mode")] {
        let db = historical_completion_protocol_db_before_v2_only().await;
        let folder = seed_folder(&db, &format!("/tmp/task-4-corrupt-header-{index}")).await;
        let parent = seed_conversation(&db, folder, AgentType::Codex).await;
        let mut document = skeleton(&format!("task-4-corrupt-header-{index}"));
        let published = publish_workflow_manifest_fixture(
            &db,
            &EventEmitter::Noop,
            parent,
            PublishWorkflowRequest {
                document: document.clone(),
            },
        )
        .await
        .unwrap();
        corrupt_protocol_header(&db, &published.workflow_id, version, mode).await;
        complete_historical_completion_protocol_migrations(&db).await;
        let before = corrupt_mutation_snapshot(&db, parent, &published.workflow_id).await;

        document.workflow_id = Some(published.workflow_id.clone());
        document.expected_manifest_revision = Some(published.manifest_revision);
        document.nodes[0].title = Some("corrupt header must not publish".into());
        let publish_error = publish_workflow_manifest_core(
            &db,
            &EventEmitter::Noop,
            parent,
            PublishWorkflowRequest { document },
        )
        .await
        .expect_err("corrupt header publication must fail closed");
        assert_eq!(publish_error.code(), "workflow_v2_retired");

        let recover_error = recover_workflow_core(
            &db,
            &EventEmitter::Noop,
            parent,
            RecoverWorkflowRequest {
                workflow_id: published.workflow_id.clone(),
                recovery_authorization_id: "must-not-consume".into(),
                expected_manifest_revision: published.manifest_revision,
                correlation_id: format!("task-4-corrupt-recover-{index}"),
            },
        )
        .await
        .expect_err("corrupt header recovery must fail closed");
        assert_eq!(recover_error.code(), "unsupported_completion_protocol");

        let manager = ConnectionManager::new();
        let connection_id = format!("task-4-corrupt-root-{index}");
        manager
            .insert_test_connection(&connection_id, AgentType::Codex, None, EventEmitter::Noop)
            .await;
        let state = manager.get_state(&connection_id).await.unwrap();
        state.write().await.conversation_id = Some(parent);
        let root_error = manager
            .send_prompt_linked_background(
                &db,
                &connection_id,
                vec![PromptInputBlock::Text {
                    text: "corrupt root must not resume".into(),
                }],
                Some(folder),
                Some(parent),
                None,
            )
            .await
            .expect_err("corrupt linked root admission must fail closed");
        assert_eq!(root_error.code(), Some("unsupported_completion_protocol"));
        assert!(!state.read().await.turn_in_flight);
        assert_eq!(
            corrupt_mutation_snapshot(&db, parent, &published.workflow_id).await,
            before
        );
    }
}

fn complete_gate_state_skeleton(token: &str) -> ManifestDocument {
    let mut document = skeleton(token);
    document.phases.push(ManifestPhase {
        id: PHASE_FINAL.into(),
        kind: Some(PHASE_FINAL.into()),
        title: None,
    });
    document.nodes.extend([
        ManifestNode {
            id: "design-reviewer".into(),
            kind: ManifestNodeKind::WorkUnit,
            phase_id: Some(PHASE_DESIGN.into()),
            role: Some(ManifestNodeRole::Reviewer),
            agent_type: Some("codex".into()),
            profile_id: None,
            task_index: None,
            work_unit_key: Some(
                build_work_unit_key(&WorkUnitKeyParts::Design {
                    rel_doc_path: "docs/superpowers/specs/task-18-capability-design.md",
                    agent_type: "codex",
                    profile_id: None,
                })
                .unwrap(),
            ),
            deps: Vec::new(),
            required: Some(true),
            node_outcome: None,
            title: None,
        },
        ManifestNode {
            id: "plan-reviewer".into(),
            kind: ManifestNodeKind::WorkUnit,
            phase_id: Some(PHASE_PLAN.into()),
            role: Some(ManifestNodeRole::Reviewer),
            agent_type: Some("codex".into()),
            profile_id: None,
            task_index: None,
            work_unit_key: Some(
                build_work_unit_key(&WorkUnitKeyParts::PlanReviewer {
                    rel_plan_path: "docs/superpowers/plans/restarted-plan.md",
                    agent_type: "codex",
                    profile_id: None,
                })
                .unwrap(),
            ),
            deps: vec!["plan-author".into()],
            required: Some(true),
            node_outcome: None,
            title: None,
        },
        ManifestNode {
            id: "final-reviewer-codex".into(),
            kind: ManifestNodeKind::WorkUnit,
            phase_id: Some(PHASE_FINAL.into()),
            role: Some(ManifestNodeRole::Reviewer),
            agent_type: Some("codex".into()),
            profile_id: None,
            task_index: None,
            work_unit_key: Some(
                build_work_unit_key(&WorkUnitKeyParts::FinalReviewer {
                    agent_type: "codex",
                    profile_id: None,
                })
                .unwrap(),
            ),
            deps: Vec::new(),
            required: Some(true),
            node_outcome: None,
            title: None,
        },
        ManifestNode {
            id: "final-reviewer-grok".into(),
            kind: ManifestNodeKind::WorkUnit,
            phase_id: Some(PHASE_FINAL.into()),
            role: Some(ManifestNodeRole::Reviewer),
            agent_type: Some("grok".into()),
            profile_id: None,
            task_index: None,
            work_unit_key: Some(
                build_work_unit_key(&WorkUnitKeyParts::FinalReviewer {
                    agent_type: "grok",
                    profile_id: None,
                })
                .unwrap(),
            ),
            deps: Vec::new(),
            required: Some(true),
            node_outcome: None,
            title: None,
        },
    ]);
    document.gates.extend([
        ManifestGate {
            id: "design".into(),
            reviewer_cohort_node_ids: vec!["design-reviewer".into()],
            required_reviewer_node_ids: vec!["design-reviewer".into()],
            resolution_mode: ResolutionMode::ParentAdjudication,
            gate_kind: Some(DocumentGateKind::Design),
        },
        ManifestGate {
            id: "plan".into(),
            reviewer_cohort_node_ids: vec!["plan-reviewer".into()],
            required_reviewer_node_ids: vec!["plan-reviewer".into()],
            resolution_mode: ResolutionMode::ParentAdjudication,
            gate_kind: Some(DocumentGateKind::Plan),
        },
    ]);
    document
}

#[tokio::test]
async fn v2_settlement_requires_gate_kind_cas_fields_and_guards_legacy_before_writes() {
    const DESIGN_REL_PATH: &str = "docs/superpowers/specs/task-18-capability-design.md";
    const PLAN_REL_PATH: &str = "docs/superpowers/plans/restarted-plan.md";
    const DESIGN_BYTES: &[u8] = b"# Design\n\nTask 3 settlement contract.\n";
    const PLAN_BYTES: &[u8] = b"# Plan\n\nTask 3 settlement contract.\n";

    let workspace = tempfile::tempdir().unwrap();
    for (rel_path, bytes) in [(DESIGN_REL_PATH, DESIGN_BYTES), (PLAN_REL_PATH, PLAN_BYTES)] {
        let path = workspace.path().join(rel_path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    let db = fresh_in_memory_db().await;
    let folder = seed_folder(&db, workspace.path().to_str().unwrap()).await;
    let parent = seed_conversation(&db, folder, AgentType::Codex).await;
    let mut document = complete_gate_state_skeleton("task-3-v2-settlement-contract");
    document.workflow_state = ManifestWorkflowState::Estimated;
    document.design = Some(DocumentRef {
        rel_path: DESIGN_REL_PATH.into(),
        digest: format!("sha256:{:x}", Sha256::digest(DESIGN_BYTES)),
    });
    document.plan = Some(DocumentRef {
        rel_path: PLAN_REL_PATH.into(),
        digest: format!("sha256:{:x}", Sha256::digest(PLAN_BYTES)),
    });
    let published = publish_workflow_manifest_fixture(
        &db,
        &EventEmitter::Noop,
        parent,
        PublishWorkflowRequest { document },
    )
    .await
    .unwrap();

    let design_error = settle_workflow_gate_v2_core(
        &db,
        &EventEmitter::Noop,
        parent,
        SettleWorkflowV2Request {
            workflow_id: published.workflow_id.clone(),
            gate_id: "design".into(),
            expected_graph_revision: published.graph_revision,
            expected_review_round: Some(1),
            expected_outcome: None,
            summary: "missing Design outcome CAS".into(),
            recovery_authorization_id: None,
        },
    )
    .await
    .unwrap_err();
    assert!(
        matches!(design_error, WorkflowStoreError::GateNotReady(ref reason) if reason.contains("Design settlement requires expected_outcome")),
        "unexpected Design validation error: {design_error:?}"
    );

    let plan_error = settle_workflow_gate_v2_core(
        &db,
        &EventEmitter::Noop,
        parent,
        SettleWorkflowV2Request {
            workflow_id: published.workflow_id.clone(),
            gate_id: "plan".into(),
            expected_graph_revision: published.graph_revision,
            expected_review_round: None,
            expected_outcome: Some(GateSettlementOutcome::Approved),
            summary: "missing Plan round CAS".into(),
            recovery_authorization_id: None,
        },
    )
    .await
    .unwrap_err();
    assert!(
        matches!(plan_error, WorkflowStoreError::GateNotReady(ref reason) if reason.contains("Plan settlement requires expected_review_round")),
        "unexpected Plan validation error: {plan_error:?}"
    );

    let legacy_parent = 101;
    let legacy_workflow_id = "wf-task-3-v2-settlement-historical";
    let legacy_db = historical_completion_protocol_db(&[historical_workflow_seed(
        legacy_workflow_id,
        legacy_parent,
        1,
        delegation_workflow::CompletionProtocolMode::V1,
        None,
    )])
    .await;
    let legacy_published = seed_historical_manifest(
        &legacy_db,
        legacy_workflow_id,
        &complete_gate_state_skeleton("task-3-v2-settlement-historical"),
    )
    .await;
    let settlements_before = delegation_workflow_gate_settlement::Entity::find()
        .filter(
            delegation_workflow_gate_settlement::Column::WorkflowId
                .eq(&legacy_published.workflow_id),
        )
        .count(&legacy_db.conn)
        .await
        .unwrap();
    let legacy_error = settle_workflow_gate_v2_core(
        &legacy_db,
        &EventEmitter::Noop,
        legacy_parent,
        SettleWorkflowV2Request {
            workflow_id: legacy_published.workflow_id.clone(),
            gate_id: "design".into(),
            expected_graph_revision: legacy_published.graph_revision,
            expected_review_round: Some(1),
            expected_outcome: Some(GateSettlementOutcome::Approved),
            summary: "historical workflow stays read-only".into(),
            recovery_authorization_id: None,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(
        legacy_error,
        WorkflowStoreError::LegacyCompletionProtocolReadOnly
    );
    assert_eq!(
        delegation_workflow_gate_settlement::Entity::find()
            .filter(
                delegation_workflow_gate_settlement::Column::WorkflowId
                    .eq(&legacy_published.workflow_id),
            )
            .count(&legacy_db.conn)
            .await
            .unwrap(),
        settlements_before
    );
}

#[tokio::test]
async fn fresh_publication_initializes_gate_state_only_for_v2_enforce() {
    const DESIGN_REL_PATH: &str = "docs/superpowers/specs/task-18-capability-design.md";
    const PLAN_REL_PATH: &str = "docs/superpowers/plans/restarted-plan.md";
    const DESIGN_BYTES: &[u8] = b"# Design\n\nTask 18 gate-state publication.\n";
    const PLAN_BYTES: &[u8] = b"# Plan\n\nTask 18 gate-state publication.\n";

    let workspace = tempfile::tempdir().unwrap();
    for (rel_path, bytes) in [(DESIGN_REL_PATH, DESIGN_BYTES), (PLAN_REL_PATH, PLAN_BYTES)] {
        let path = workspace.path().join(rel_path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    let db = fresh_in_memory_db().await;
    let folder = seed_folder(&db, workspace.path().to_str().unwrap()).await;
    let publish = |parent: i32, token: &str| {
        let mut document = complete_gate_state_skeleton(token);
        document.workflow_state = ManifestWorkflowState::Estimated;
        document.design = Some(DocumentRef {
            rel_path: DESIGN_REL_PATH.into(),
            digest: format!("sha256:{:x}", Sha256::digest(DESIGN_BYTES)),
        });
        document.plan = Some(DocumentRef {
            rel_path: PLAN_REL_PATH.into(),
            digest: format!("sha256:{:x}", Sha256::digest(PLAN_BYTES)),
        });
        (parent, PublishWorkflowRequest { document })
    };

    let enforce_parent = seed_conversation(&db, folder, AgentType::Codex).await;
    let (parent, request) = publish(enforce_parent, "task-18-enforce-gate-state");
    let enforced = publish_workflow_manifest_fixture(&db, &EventEmitter::Noop, parent, request)
        .await
        .unwrap();
    let states = delegation_workflow_gate_state::Entity::find()
        .filter(delegation_workflow_gate_state::Column::WorkflowId.eq(&enforced.workflow_id))
        .all(&db.conn)
        .await
        .unwrap();
    assert_eq!(
        states.len(),
        3,
        "Design, Plan, and Final need durable state"
    );
    let expected = [
        ("design", BTreeSet::from(["design-reviewer"])),
        ("plan", BTreeSet::from(["plan-reviewer"])),
        (
            "final",
            BTreeSet::from(["final-reviewer-codex", "final-reviewer-grok"]),
        ),
    ];
    for (gate_id, selected) in expected {
        let state = states
            .iter()
            .find(|state| state.gate_id == gate_id)
            .unwrap();
        assert_eq!(state.current_review_round, 1);
        assert_eq!(state.gate_lineage.len(), 71);
        assert!(state.gate_lineage.starts_with("sha256:"));
        assert_eq!(
            serde_json::from_str::<BTreeSet<&str>>(&state.selected_node_ids_json).unwrap(),
            selected
        );
    }

    let initial_design_lineage = states
        .iter()
        .find(|state| state.gate_id == "design")
        .unwrap()
        .gate_lineage
        .clone();
    let initial_plan_lineage = states
        .iter()
        .find(|state| state.gate_id == "plan")
        .unwrap()
        .gate_lineage
        .clone();
    let initial_final_lineage = states
        .iter()
        .find(|state| state.gate_id == "final")
        .unwrap()
        .gate_lineage
        .clone();
    let plan_state = states
        .iter()
        .find(|state| state.gate_id == "plan")
        .unwrap()
        .clone();
    let mut plan_state: delegation_workflow_gate_state::ActiveModel = plan_state.into();
    plan_state.current_review_round = Set(2);
    plan_state.selected_node_ids_json = Set("[]".into());
    plan_state.update(&db.conn).await.unwrap();

    let mut title_only = complete_gate_state_skeleton("task-18-enforce-gate-state");
    title_only.workflow_id = Some(enforced.workflow_id.clone());
    title_only.expected_manifest_revision = Some(1);
    title_only.workflow_state = ManifestWorkflowState::Estimated;
    title_only.design = Some(DocumentRef {
        rel_path: DESIGN_REL_PATH.into(),
        digest: format!("sha256:{:x}", Sha256::digest(DESIGN_BYTES)),
    });
    title_only.plan = Some(DocumentRef {
        rel_path: PLAN_REL_PATH.into(),
        digest: format!("sha256:{:x}", Sha256::digest(PLAN_BYTES)),
    });
    title_only
        .nodes
        .iter_mut()
        .find(|node| node.id == "plan-reviewer")
        .unwrap()
        .title = Some("Display-only reviewer title".into());
    let title_result = publish_workflow_manifest_fixture(
        &db,
        &EventEmitter::Noop,
        enforce_parent,
        PublishWorkflowRequest {
            document: title_only.clone(),
        },
    )
    .await
    .unwrap();
    let preserved_plan = delegation_workflow_gate_state::Entity::find_by_id((
        enforced.workflow_id.clone(),
        "plan".to_string(),
    ))
    .one(&db.conn)
    .await
    .unwrap()
    .unwrap();
    assert_eq!(preserved_plan.gate_lineage, initial_plan_lineage);
    assert_eq!(preserved_plan.current_review_round, 2);
    assert_eq!(preserved_plan.selected_node_ids_json, "[]");

    const PLAN_BYTES_V2: &[u8] = b"# Plan\n\nTask 18 changed gate-state material.\n";
    std::fs::write(workspace.path().join(PLAN_REL_PATH), PLAN_BYTES_V2).unwrap();
    title_only.expected_manifest_revision = Some(title_result.manifest_revision);
    title_only.plan = Some(DocumentRef {
        rel_path: PLAN_REL_PATH.into(),
        digest: format!("sha256:{:x}", Sha256::digest(PLAN_BYTES_V2)),
    });
    publish_workflow_manifest_fixture(
        &db,
        &EventEmitter::Noop,
        enforce_parent,
        PublishWorkflowRequest {
            document: title_only,
        },
    )
    .await
    .unwrap();
    let rotated_states = delegation_workflow_gate_state::Entity::find()
        .filter(delegation_workflow_gate_state::Column::WorkflowId.eq(&enforced.workflow_id))
        .all(&db.conn)
        .await
        .unwrap();
    let rotated_design = rotated_states
        .iter()
        .find(|state| state.gate_id == "design")
        .unwrap();
    let rotated_plan = rotated_states
        .iter()
        .find(|state| state.gate_id == "plan")
        .unwrap();
    let rotated_final = rotated_states
        .iter()
        .find(|state| state.gate_id == "final")
        .unwrap();
    assert_eq!(rotated_design.gate_lineage, initial_design_lineage);
    assert_ne!(rotated_plan.gate_lineage, initial_plan_lineage);
    assert_ne!(rotated_final.gate_lineage, initial_final_lineage);
    for state in [rotated_plan, rotated_final] {
        assert_eq!(state.current_review_round, 1);
        assert_ne!(state.selected_node_ids_json, "[]");
    }

    let reviewer_task_id = "task-18-published-design-reviewer";
    admit_v2_fixture_run(
        &db,
        enforce_parent,
        seed_conversation(&db, folder, AgentType::Codex).await,
        workspace.path(),
        reviewer_task_id,
        "codex",
        build_work_unit_key(&WorkUnitKeyParts::Design {
            rel_doc_path: DESIGN_REL_PATH,
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap(),
        "Review the published Design",
    )
    .await;
    let reviewer_binding = delegation_workflow_run_binding::Entity::find_by_id(reviewer_task_id)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    let design_state = states
        .iter()
        .find(|state| state.gate_id == "design")
        .unwrap();
    assert_eq!(reviewer_binding.gate_id.as_deref(), Some("design"));
    assert_eq!(
        reviewer_binding.gate_lineage.as_deref(),
        Some(design_state.gate_lineage.as_str())
    );
    assert_eq!(reviewer_binding.review_round, Some(1));
}

#[tokio::test]
async fn roster_only_republication_selects_only_added_reviewers_and_retires_removed_ones() {
    const DESIGN_REL_PATH: &str = "docs/superpowers/specs/task-18-capability-design.md";
    const PLAN_REL_PATH: &str = "docs/superpowers/plans/restarted-plan.md";
    const DESIGN_BYTES: &[u8] = b"# Design\n\nTask 18 roster transition.\n";
    const PLAN_BYTES: &[u8] = b"# Plan\n\nTask 18 roster transition.\n";

    let workspace = tempfile::tempdir().unwrap();
    for (rel_path, bytes) in [(DESIGN_REL_PATH, DESIGN_BYTES), (PLAN_REL_PATH, PLAN_BYTES)] {
        let path = workspace.path().join(rel_path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    let db = fresh_in_memory_db().await;
    let folder = seed_folder(&db, workspace.path().to_str().unwrap()).await;
    let parent = seed_conversation(&db, folder, AgentType::Codex).await;
    let mut document = complete_gate_state_skeleton("task-18-roster-transition");
    document.workflow_state = ManifestWorkflowState::Estimated;
    document.design = Some(DocumentRef {
        rel_path: DESIGN_REL_PATH.into(),
        digest: format!("sha256:{:x}", Sha256::digest(DESIGN_BYTES)),
    });
    document.plan = Some(DocumentRef {
        rel_path: PLAN_REL_PATH.into(),
        digest: format!("sha256:{:x}", Sha256::digest(PLAN_BYTES)),
    });
    let published = publish_workflow_manifest_fixture(
        &db,
        &EventEmitter::Noop,
        parent,
        PublishWorkflowRequest {
            document: document.clone(),
        },
    )
    .await
    .unwrap();
    let initial_states = delegation_workflow_gate_state::Entity::find()
        .filter(delegation_workflow_gate_state::Column::WorkflowId.eq(&published.workflow_id))
        .all(&db.conn)
        .await
        .unwrap();
    let initial_lineage = |gate_id: &str| {
        initial_states
            .iter()
            .find(|state| state.gate_id == gate_id)
            .unwrap()
            .gate_lineage
            .clone()
    };

    document.workflow_id = Some(published.workflow_id.clone());
    document.expected_manifest_revision = Some(published.manifest_revision);
    document.publication_token.push_str("-add");
    document.nodes.extend([
        ManifestNode {
            id: "plan-reviewer-grok".into(),
            kind: ManifestNodeKind::WorkUnit,
            phase_id: Some(PHASE_PLAN.into()),
            role: Some(ManifestNodeRole::Reviewer),
            agent_type: Some("grok".into()),
            profile_id: None,
            task_index: None,
            work_unit_key: Some(
                build_work_unit_key(&WorkUnitKeyParts::PlanReviewer {
                    rel_plan_path: PLAN_REL_PATH,
                    agent_type: "grok",
                    profile_id: None,
                })
                .unwrap(),
            ),
            deps: vec!["plan-author".into()],
            required: Some(true),
            node_outcome: None,
            title: None,
        },
        ManifestNode {
            id: "final-reviewer-extra".into(),
            kind: ManifestNodeKind::WorkUnit,
            phase_id: Some(PHASE_FINAL.into()),
            role: Some(ManifestNodeRole::Reviewer),
            agent_type: Some("codex".into()),
            profile_id: Some("task-18-extra".into()),
            task_index: None,
            work_unit_key: Some(
                build_work_unit_key(&WorkUnitKeyParts::FinalReviewer {
                    agent_type: "codex",
                    profile_id: Some("task-18-extra"),
                })
                .unwrap(),
            ),
            deps: Vec::new(),
            required: Some(true),
            node_outcome: None,
            title: None,
        },
    ]);
    let plan_gate = document
        .gates
        .iter_mut()
        .find(|gate| gate.id == PHASE_PLAN)
        .unwrap();
    plan_gate
        .reviewer_cohort_node_ids
        .push("plan-reviewer-grok".into());
    plan_gate
        .required_reviewer_node_ids
        .push("plan-reviewer-grok".into());
    let added = publish_workflow_manifest_fixture(
        &db,
        &EventEmitter::Noop,
        parent,
        PublishWorkflowRequest {
            document: document.clone(),
        },
    )
    .await
    .unwrap();
    let added_states = delegation_workflow_gate_state::Entity::find()
        .filter(delegation_workflow_gate_state::Column::WorkflowId.eq(&published.workflow_id))
        .all(&db.conn)
        .await
        .unwrap();
    for (gate_id, added_reviewer) in [
        (PHASE_PLAN, "plan-reviewer-grok"),
        (PHASE_FINAL, "final-reviewer-extra"),
    ] {
        let state = added_states
            .iter()
            .find(|state| state.gate_id == gate_id)
            .unwrap();
        assert_eq!(state.gate_lineage, initial_lineage(gate_id));
        assert_eq!(state.current_review_round, 2);
        assert_eq!(
            serde_json::from_str::<BTreeSet<&str>>(&state.selected_node_ids_json).unwrap(),
            BTreeSet::from([added_reviewer])
        );
    }

    document.expected_manifest_revision = Some(added.manifest_revision);
    document.publication_token.push_str("-remove");
    document
        .nodes
        .retain(|node| node.id != "plan-reviewer-grok" && node.id != "final-reviewer-extra");
    let plan_gate = document
        .gates
        .iter_mut()
        .find(|gate| gate.id == PHASE_PLAN)
        .unwrap();
    plan_gate
        .reviewer_cohort_node_ids
        .retain(|node_id| node_id != "plan-reviewer-grok");
    plan_gate
        .required_reviewer_node_ids
        .retain(|node_id| node_id != "plan-reviewer-grok");
    publish_workflow_manifest_fixture(
        &db,
        &EventEmitter::Noop,
        parent,
        PublishWorkflowRequest { document },
    )
    .await
    .unwrap();
    let removed_states = delegation_workflow_gate_state::Entity::find()
        .filter(delegation_workflow_gate_state::Column::WorkflowId.eq(&published.workflow_id))
        .all(&db.conn)
        .await
        .unwrap();
    for gate_id in [PHASE_PLAN, PHASE_FINAL] {
        let state = removed_states
            .iter()
            .find(|state| state.gate_id == gate_id)
            .unwrap();
        assert_eq!(state.gate_lineage, initial_lineage(gate_id));
        assert_eq!(state.current_review_round, 2);
        assert_eq!(state.selected_node_ids_json, "[]");
    }
}

#[tokio::test]
async fn roster_only_final_republication_delivers_after_add_and_remove() {
    const DESIGN_REL_PATH: &str = "docs/superpowers/specs/task-18-final-roster-design.md";
    const PLAN_REL_PATH: &str = "docs/superpowers/plans/restarted-plan.md";
    const DESIGN_BYTES: &[u8] = b"# Design\n\nTask 18 Final roster delivery.\n";
    const PLAN_BYTES: &[u8] = b"## Global Constraints\n\n- Task 18 Final roster delivery.\n";

    let repo = tempfile::tempdir().expect("Task 18 Final roster repo");
    git_fixture(repo.path(), &["init", "--quiet"]);
    for (rel_path, bytes) in [(DESIGN_REL_PATH, DESIGN_BYTES), (PLAN_REL_PATH, PLAN_BYTES)] {
        let path = repo.path().join(rel_path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    git_fixture(repo.path(), &["add", "."]);
    git_fixture(
        repo.path(),
        &[
            "-c",
            "user.name=Codeg Test",
            "-c",
            "user.email=codeg@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "final roster fixture",
        ],
    );

    let db = fresh_in_memory_db().await;
    let folder = seed_folder(&db, repo.path().to_str().unwrap()).await;
    let parent = seed_conversation(&db, folder, AgentType::Codex).await;
    let mut document = final_review_skeleton("task-18-final-roster-delivery");
    document.workflow_state = ManifestWorkflowState::Estimated;
    document.design = Some(DocumentRef {
        rel_path: DESIGN_REL_PATH.into(),
        digest: format!("sha256:{:x}", Sha256::digest(DESIGN_BYTES)),
    });
    document.plan = Some(DocumentRef {
        rel_path: PLAN_REL_PATH.into(),
        digest: format!("sha256:{:x}", Sha256::digest(PLAN_BYTES)),
    });
    let published = publish_workflow_manifest_fixture(
        &db,
        &EventEmitter::Noop,
        parent,
        PublishWorkflowRequest {
            document: document.clone(),
        },
    )
    .await
    .unwrap();

    let plan_author_task_id = "task-18-final-roster-plan-author";
    admit_v2_fixture_run(
        &db,
        parent,
        seed_conversation(&db, folder, AgentType::Codex).await,
        repo.path(),
        plan_author_task_id,
        "codex",
        build_work_unit_key(&WorkUnitKeyParts::PlanAuthor {
            rel_plan_path: PLAN_REL_PATH,
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap(),
        "Task 18 Final roster Plan Author",
    )
    .await;
    materialize_v2_fixture_run(
        &db,
        plan_author_task_id,
        "Plan authored.\n\nConclusion: done",
    )
    .await;
    let plan_reviewer_task_id = "task-18-final-roster-plan-reviewer";
    admit_v2_fixture_run(
        &db,
        parent,
        seed_conversation(&db, folder, AgentType::Codex).await,
        repo.path(),
        plan_reviewer_task_id,
        "codex",
        build_work_unit_key(&WorkUnitKeyParts::PlanReviewer {
            rel_plan_path: PLAN_REL_PATH,
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap(),
        "Task 18 Final roster Plan Reviewer",
    )
    .await;
    materialize_v2_fixture_run(
        &db,
        plan_reviewer_task_id,
        "Plan review passed.\n\nConclusion: approve",
    )
    .await;
    let current = delegation_workflow::Entity::find_by_id(&published.workflow_id)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    settle_workflow_gate_v2_core(
        &db,
        &EventEmitter::Noop,
        parent,
        SettleWorkflowV2Request {
            workflow_id: published.workflow_id.clone(),
            gate_id: PHASE_PLAN.into(),
            expected_graph_revision: current.graph_revision as u64,
            expected_review_round: Some(1),
            expected_outcome: Some(GateSettlementOutcome::Approved),
            summary: "Task 18 Final roster Plan approval".into(),
            recovery_authorization_id: None,
        },
    )
    .await
    .unwrap();

    let codex_task_id = "task-18-final-roster-codex";
    admit_v2_fixture_run(
        &db,
        parent,
        seed_conversation(&db, folder, AgentType::Codex).await,
        repo.path(),
        codex_task_id,
        "codex",
        build_work_unit_key(&WorkUnitKeyParts::FinalReviewer {
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap(),
        "Task 18 retained Codex Final review",
    )
    .await;
    materialize_v2_fixture_run(
        &db,
        codex_task_id,
        "Final review passed.\n\nConclusion: approve",
    )
    .await;
    let grok_task_id = "task-18-final-roster-grok";
    admit_v2_fixture_run(
        &db,
        parent,
        seed_conversation(&db, folder, AgentType::Grok).await,
        repo.path(),
        grok_task_id,
        "grok",
        build_work_unit_key(&WorkUnitKeyParts::FinalReviewer {
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap(),
        "Task 18 retained Grok Final review",
    )
    .await;
    materialize_v2_fixture_run(
        &db,
        grok_task_id,
        "Independent Final review passed.\n\nConclusion: approve",
    )
    .await;

    let header = delegation_workflow::Entity::find_by_id(&published.workflow_id)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    document.workflow_id = Some(published.workflow_id.clone());
    document.expected_manifest_revision = Some(header.active_manifest_revision as u64);
    document.publication_token.push_str("-add");
    document.workflow_state = ManifestWorkflowState::Approved;
    document.nodes.push(ManifestNode {
        id: "final-reviewer-extra".into(),
        kind: ManifestNodeKind::WorkUnit,
        phase_id: Some(PHASE_FINAL.into()),
        role: Some(ManifestNodeRole::Reviewer),
        agent_type: Some("gemini".into()),
        profile_id: None,
        task_index: None,
        work_unit_key: Some(
            build_work_unit_key(&WorkUnitKeyParts::FinalReviewer {
                agent_type: "gemini",
                profile_id: None,
            })
            .unwrap(),
        ),
        deps: Vec::new(),
        required: Some(true),
        node_outcome: None,
        title: None,
    });
    let added = publish_workflow_manifest_fixture(
        &db,
        &EventEmitter::Noop,
        parent,
        PublishWorkflowRequest {
            document: document.clone(),
        },
    )
    .await
    .unwrap();
    let added_state = delegation_workflow_gate_state::Entity::find_by_id((
        published.workflow_id.clone(),
        PHASE_FINAL.to_string(),
    ))
    .one(&db.conn)
    .await
    .unwrap()
    .unwrap();
    assert_eq!(added_state.current_review_round, 2);
    assert_eq!(
        added_state.selected_node_ids_json,
        r#"["final-reviewer-extra"]"#
    );

    // Isolate Final delivery after the roster-triggered Plan reapproval prerequisite.
    let header = delegation_workflow::Entity::find_by_id(&published.workflow_id)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    let mut header: delegation_workflow::ActiveModel = header.into();
    header.workflow_state = Set(delegation_workflow::WorkflowState::Approved);
    header.update(&db.conn).await.unwrap();

    let extra_task_id = "task-18-final-roster-extra";
    admit_v2_fixture_run(
        &db,
        parent,
        seed_conversation(&db, folder, AgentType::Gemini).await,
        repo.path(),
        extra_task_id,
        "gemini",
        build_work_unit_key(&WorkUnitKeyParts::FinalReviewer {
            agent_type: "gemini",
            profile_id: None,
        })
        .unwrap(),
        "Task 18 added Final review",
    )
    .await;
    materialize_v2_fixture_run(
        &db,
        extra_task_id,
        "Added Final review passed.\n\nConclusion: approve",
    )
    .await;
    // Production listener/root paths go through preflight request construction
    // (`current_final_delivery_request`), not a hand-built guard request.
    let ready_after_add_task =
        guard_task_final_delivery_core(&db, &EventEmitter::Noop, extra_task_id)
            .await
            .expect("production task delivery path must not fail after roster add");
    assert!(
        matches!(
            ready_after_add_task,
            Some(FinalDeliveryGuardResult::Ready(_))
        ),
        "selected Final reviewer must reach delivery via guard_task_final_delivery_core after roster add; got {ready_after_add_task:?}"
    );
    let ready_after_add_current = guard_current_final_delivery_core(
        &db,
        &EventEmitter::Noop,
        parent,
        Some(&published.workflow_id),
    )
    .await
    .expect("production current delivery path must not fail after roster add");
    assert!(
        matches!(
            ready_after_add_current,
            Some(FinalDeliveryGuardResult::Ready(_))
        ),
        "root path must evaluate selective Final delivery after roster add; got {ready_after_add_current:?}"
    );

    document.expected_manifest_revision = Some(added.manifest_revision);
    document.publication_token.push_str("-remove");
    let removed_reviewer = document
        .nodes
        .iter_mut()
        .find(|node| node.id == "final-reviewer-extra")
        .unwrap();
    removed_reviewer.required = Some(false);
    removed_reviewer.node_outcome =
        Some(codeg_lib::acp::delegation::workflow::ManifestNodeOutcome::Canceled);
    publish_workflow_manifest_fixture(
        &db,
        &EventEmitter::Noop,
        parent,
        PublishWorkflowRequest { document },
    )
    .await
    .unwrap();
    let removed_state = delegation_workflow_gate_state::Entity::find_by_id((
        published.workflow_id.clone(),
        PHASE_FINAL.to_string(),
    ))
    .one(&db.conn)
    .await
    .unwrap()
    .unwrap();
    assert_eq!(removed_state.current_review_round, 2);
    assert_eq!(removed_state.selected_node_ids_json, "[]");

    let ready_after_remove_task =
        guard_task_final_delivery_core(&db, &EventEmitter::Noop, codex_task_id)
            .await
            .expect("production task delivery path must not fail after roster remove");
    assert!(
        matches!(
            ready_after_remove_task,
            Some(FinalDeliveryGuardResult::Ready(_))
        ),
        "retained earlier-round Final reviewer must reach delivery via guard_task_final_delivery_core after roster remove; got {ready_after_remove_task:?}"
    );
    let ready_after_remove_current = guard_current_final_delivery_core(
        &db,
        &EventEmitter::Noop,
        parent,
        Some(&published.workflow_id),
    )
    .await
    .expect("production current delivery path must not fail after roster remove");
    assert!(
        matches!(
            ready_after_remove_current,
            Some(FinalDeliveryGuardResult::Ready(_))
        ),
        "root path must evaluate selective Final delivery after roster remove; got {ready_after_remove_current:?}"
    );
}

#[derive(Clone, Copy, Debug)]
enum CapabilityCase {
    ToolCompleteWork,
    TerminalConclusionOnly,
    ReportConclusionOnly,
    AmbiguousThenUserAdjudication,
    ObsoleteCardPlusNaturalConclusion,
}

const HISTORICAL_WORKFLOW_ROOT_FEATURES: CompanionFeatures = CompanionFeatures {
    delegation: true,
    coordination_v1: false,
    feedback: false,
    ask: false,
    sessions: false,
    workflow_v2: true,
    completion_v2: false,
};

const HISTORICAL_COMPLETION_CHILD_FEATURES: CompanionFeatures = CompanionFeatures {
    delegation: false,
    coordination_v1: false,
    feedback: false,
    ask: false,
    sessions: false,
    workflow_v2: false,
    completion_v2: true,
};

struct CapabilityResult {
    child_run_count: u64,
    card_summary_json: Option<String>,
    completion: CompletionCardV2,
    desktop_completion: Value,
    server_completion: Value,
    mcp_completion: Value,
    tool_intent_count: u64,
}

#[allow(clippy::too_many_arguments)]
async fn admit_v2_fixture_run(
    db: &codeg_lib::db::AppDatabase,
    parent: i32,
    child: i32,
    workspace: &Path,
    task_id: &str,
    agent_type: &str,
    work_unit_key: String,
    task_preview: &str,
) {
    let runs = Arc::new(RunStore::new(Arc::new(codeg_lib::db::AppDatabase {
        conn: db.conn.clone(),
    })));
    with_historical_workflow_fixture_mutations(runs.admit_gen1_reserving(ReservingRunInsert {
        orchestration_binding: None,
        task_id: task_id.into(),
        root_task_id: task_id.into(),
        previous_task_id: None,
        generation: 1,
        parent_conversation_id: parent,
        parent_tool_use_id: Some(format!("tool-{task_id}")),
        child_conversation_id: child,
        agent_type: agent_type.into(),
        profile_id: None,
        workspace_path: Some(workspace.to_string_lossy().into_owned()),
        route_fingerprint: Some(format!("route-{task_id}")),
        launch_snapshot_version: Some("v1".into()),
        mode_id: None,
        config_values_json: Some("{}".into()),
        task_preview: Some(task_preview.into()),
        request_fingerprint: Some(format!("fingerprint-{task_id}")),
        admission_class: delegation_task_run::AdmissionClass::NormalRevision,
        lineage_root_task_id: task_id.into(),
        work_unit_key: Some(work_unit_key),
        history_only: false,
        replaced_task_id: None,
        replacement_reason: None,
        started_at: Some(chrono::Utc::now()),
    }))
    .await
    .unwrap_or_else(|error| panic!("admit {task_id}: {error:?}"));
}

async fn materialize_v2_fixture_run(
    db: &codeg_lib::db::AppDatabase,
    task_id: &str,
    final_assistant_text: &str,
) {
    let run = delegation_task_run::Entity::find_by_id(task_id)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    let mut run: delegation_task_run::ActiveModel = run.into();
    run.status = Set(delegation_task_run::DelegationRunStatus::Completed);
    run.reached_running_at = Set(Some(chrono::Utc::now()));
    run.finished_at = Set(Some(chrono::Utc::now()));
    run.update(&db.conn).await.unwrap();
    let txn = db.conn.begin().await.unwrap();
    let terminal = materialize_terminal_completion_txn(
        &txn,
        TerminalCompletionInput {
            task_id: task_id.into(),
            terminal_status: delegation_task_run::DelegationRunStatus::Completed,
            final_assistant_text: final_assistant_text.into(),
            pre_read_reports: Vec::new(),
            pre_read_artifact: None,
        },
    )
    .await
    .unwrap();
    assert!(terminal.evidence.is_some());
    txn.commit().await.unwrap();
}

async fn run_capability_case(case: CapabilityCase) -> CapabilityResult {
    const DESIGN_REL_PATH: &str = "docs/superpowers/specs/task-18-capability-design.md";
    const DESIGN_BYTES: &[u8] = b"# Design\n\nPlatform completion capability matrix.\n";

    let workspace = tempfile::tempdir().expect("capability workspace");
    let workspace_path = workspace.path().to_path_buf();
    let design_path = workspace_path.join(DESIGN_REL_PATH);
    std::fs::create_dir_all(design_path.parent().unwrap()).unwrap();
    std::fs::write(&design_path, DESIGN_BYTES).unwrap();
    let plan_path = workspace_path.join("docs/superpowers/plans/restarted-plan.md");
    std::fs::create_dir_all(plan_path.parent().unwrap()).unwrap();
    std::fs::write(&plan_path, b"# Plan\n\nTask 18 capability fixture.\n").unwrap();

    let db = fresh_in_memory_db().await;
    let folder = seed_folder(&db, workspace_path.to_str().unwrap()).await;
    let parent = seed_conversation(&db, folder, AgentType::Codex).await;
    let child = seed_conversation(&db, folder, AgentType::Codex).await;
    let token = format!("task-18-capability-{}", uuid::Uuid::new_v4());
    let mut document = skeleton(&token);
    document.design = Some(DocumentRef {
        rel_path: DESIGN_REL_PATH.into(),
        digest: format!("sha256:{:x}", Sha256::digest(DESIGN_BYTES)),
    });
    let published = publish_workflow_manifest_fixture(
        &db,
        &EventEmitter::Noop,
        parent,
        PublishWorkflowRequest { document },
    )
    .await
    .unwrap();
    let workflow = delegation_workflow::Entity::find_by_id(&published.workflow_id)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    let mut workflow: delegation_workflow::ActiveModel = workflow.into();
    workflow.completion_protocol_version = Set(2);
    workflow.completion_protocol_mode = Set(delegation_workflow::CompletionProtocolMode::V2Enforce);
    workflow.update(&db.conn).await.unwrap();

    let author_key = build_work_unit_key(&WorkUnitKeyParts::PlanAuthor {
        rel_plan_path: "docs/superpowers/plans/restarted-plan.md",
        agent_type: "codex",
        profile_id: None,
    })
    .unwrap();
    let task_id = format!("task-18-capability-{}", uuid::Uuid::new_v4());
    let runs = Arc::new(RunStore::new(Arc::new(codeg_lib::db::AppDatabase {
        conn: db.conn.clone(),
    })));
    with_historical_workflow_fixture_mutations(runs.admit_gen1_reserving(ReservingRunInsert {
        orchestration_binding: None,
        task_id: task_id.clone(),
        root_task_id: task_id.clone(),
        previous_task_id: None,
        generation: 1,
        parent_conversation_id: parent,
        parent_tool_use_id: Some(format!("tool-{task_id}")),
        child_conversation_id: child,
        agent_type: "codex".into(),
        profile_id: None,
        workspace_path: Some(workspace_path.to_string_lossy().into_owned()),
        route_fingerprint: Some("task-18-capability-route".into()),
        launch_snapshot_version: Some("v1".into()),
        mode_id: None,
        config_values_json: Some("{}".into()),
        task_preview: Some("Task 18 capability".into()),
        request_fingerprint: Some(format!("fp-{task_id}")),
        admission_class: delegation_task_run::AdmissionClass::NormalRevision,
        lineage_root_task_id: task_id.clone(),
        work_unit_key: Some(author_key),
        history_only: false,
        replaced_task_id: None,
        replacement_reason: None,
        started_at: Some(chrono::Utc::now()),
    }))
    .await
    .unwrap();

    let child_connection_id = format!("task-18-capability-child-{task_id}");
    runs.bind_child_connection_while_reserving(&task_id, &child_connection_id)
        .await
        .unwrap();
    with_historical_workflow_fixture_mutations(runs.promote_running(
        &task_id,
        &child_connection_id,
        chrono::Utc::now(),
    ))
    .await
    .unwrap();

    let broker = Arc::new(
        DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            Arc::new(RootDepth) as Arc<dyn ConversationDepthLookup>,
        )
        .with_run_store(Arc::clone(&runs)),
    );
    let tokens = Arc::new(TokenRegistry::default());
    let root_token = format!("task-18-capability-root-{task_id}");
    let child_token = format!("task-18-capability-child-token-{task_id}");
    let parent_connection_id = format!("task-18-capability-parent-{task_id}");
    tokens
        .register(
            root_token.clone(),
            TokenEntry {
                parent_connection_id: parent_connection_id.clone(),
                working_dir: workspace_path.clone(),
                coordination_v1: false,
                delegation_continuation_v1: false,
                role: CompanionRole::Root,
                workflow_v2: true,
                completion_v2: false,
                bound_task_id: None,
            },
        )
        .await;
    tokens
        .register(
            child_token.clone(),
            TokenEntry {
                parent_connection_id: child_connection_id.clone(),
                working_dir: workspace_path.clone(),
                coordination_v1: false,
                delegation_continuation_v1: false,
                role: CompanionRole::DelegationChild,
                workflow_v2: false,
                completion_v2: true,
                bound_task_id: Some(task_id.clone()),
            },
        )
        .await;
    let listener = DelegationListener::new_with_workflow_emitter(
        Arc::clone(&broker),
        tokens,
        Arc::new(CompanionLeaseRegistry::default()),
        Arc::new(FixedParent(parent)) as Arc<dyn ParentSessionLookup>,
        Arc::new(NoFeedback),
        Arc::new(NoQuestions),
        Arc::new(NoSessionInfo),
        codeg_lib::acp::delegation::wait_cancel::WaitCancelRegistry::new_shared(),
        EventEmitter::Noop,
    );
    let socket_path = workflow_socket_path();
    let listener_task = tokio::spawn(listener.run(socket_path.clone()));
    let readiness =
        wait_for_workflow_listener(&socket_path, &root_token, &published.workflow_id).await;
    assert!(
        readiness.get("error").is_none(),
        "listener readiness: {readiness}"
    );
    let child_companion = CompanionContext {
        parent_connection_id: child_connection_id.clone(),
        socket_path: socket_path.to_string_lossy().into_owned(),
        token: child_token,
        features: HISTORICAL_COMPLETION_CHILD_FEATURES,
        role: CompanionRole::DelegationChild,
        connection_incarnation_id: format!("incarnation-{task_id}"),
        disabled_agents: Vec::new(),
    };
    let root_companion = CompanionContext {
        parent_connection_id,
        socket_path: socket_path.to_string_lossy().into_owned(),
        token: root_token,
        features: HISTORICAL_WORKFLOW_ROOT_FEATURES,
        role: CompanionRole::Root,
        connection_incarnation_id: format!("root-incarnation-{task_id}"),
        disabled_agents: Vec::new(),
    };
    assert!(companion_tool_names(&child_companion)
        .await
        .iter()
        .any(|name| name == "complete_work"));
    assert!(!companion_tool_names(&root_companion)
        .await
        .iter()
        .any(|name| name == "complete_work"));

    if matches!(case, CapabilityCase::ToolCompleteWork) {
        let request = CompleteWorkRequest {
            outcome: CompletionOutcome::Done,
            summary: Some("tool completion".into()),
            report_file: None,
        };
        let stable_tool_call_id = format!("complete-work-{task_id}");
        let first = with_historical_workflow_fixture_mutations(accept_complete_work_txn(
            runs.db(),
            &task_id,
            &child_connection_id,
            &stable_tool_call_id,
            &request,
        ))
        .await
        .unwrap();
        let replay = with_historical_workflow_fixture_mutations(accept_complete_work_txn(
            runs.db(),
            &task_id,
            &child_connection_id,
            &stable_tool_call_id,
            &request,
        ))
        .await
        .unwrap();
        assert_eq!(first.intent_id, replay.intent_id);
        assert_eq!(first.accepted_ordinal, 1);
    }

    let run = delegation_task_run::Entity::find_by_id(&task_id)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    let mut run: delegation_task_run::ActiveModel = run.into();
    run.status = Set(delegation_task_run::DelegationRunStatus::Completed);
    run.finished_at = Set(Some(chrono::Utc::now()));
    run.card_summary_json = Set(
        matches!(case, CapabilityCase::ObsoleteCardPlusNaturalConclusion)
            .then(|| r#"{"kind":"author","status":"done","plan_digest":"model"}"#.into()),
    );
    run.update(&db.conn).await.unwrap();

    let final_assistant_text: String = match case {
        CapabilityCase::ToolCompleteWork => "Tool completion submitted.".into(),
        CapabilityCase::TerminalConclusionOnly => {
            "Implementation complete.\n\nConclusion: done".into()
        }
        CapabilityCase::ReportConclusionOnly => {
            let report_path = workspace_path.join("reports/task-18.md");
            std::fs::create_dir_all(report_path.parent().unwrap()).unwrap();
            std::fs::write(&report_path, "# Task 18 report\n\nConclusion: done\n").unwrap();
            "See [the report](reports/task-18.md).".into()
        }
        CapabilityCase::AmbiguousThenUserAdjudication => {
            "Implemented the requested changes without an explicit conclusion.".into()
        }
        CapabilityCase::ObsoleteCardPlusNaturalConclusion => {
            "```json\n{\"kind\":\"author\",\"status\":\"done\",\"plan_digest\":\"model\"}\n```\n\nConclusion: done"
                .into()
        }
    };
    let pre_read_reports =
        pre_read_completion_reports_for_test(&final_assistant_text, &[], Some(&workspace_path))
            .await;
    assert_eq!(
        pre_read_reports.len(),
        usize::from(matches!(case, CapabilityCase::ReportConclusionOnly)),
        "{case:?}"
    );
    let txn = db.conn.begin().await.unwrap();
    let terminal = materialize_terminal_completion_txn(
        &txn,
        TerminalCompletionInput {
            task_id: task_id.clone(),
            terminal_status: delegation_task_run::DelegationRunStatus::Completed,
            final_assistant_text,
            pre_read_reports,
            pre_read_artifact: None,
        },
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();

    let static_dir = tempfile::tempdir().unwrap();
    let state = Arc::new(AppState::new_for_test(
        codeg_lib::db::AppDatabase {
            conn: db.conn.clone(),
        },
        workspace_path.clone(),
    ));
    let server = TestServer::new(build_router(
        state.clone(),
        "task-18-token".into(),
        static_dir.path().to_path_buf(),
        Arc::new(ShutdownSignal::new()),
    ))
    .unwrap();
    if matches!(case, CapabilityCase::AmbiguousThenUserAdjudication) {
        with_historical_workflow_fixture_mutations(resolve_completion_decision_txn(
            &db,
            parent,
            terminal.attention.expect("ambiguous completion decision"),
            CompletionOutcome::Done,
            "task-18-capability-adjudication",
        ))
        .await
        .unwrap();
    }

    let stored_run = delegation_task_run::Entity::find_by_id(&task_id)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    let runs = delegation_task_run::Entity::find()
        .filter(delegation_task_run::Column::ParentConversationId.eq(parent))
        .all(&db.conn)
        .await
        .unwrap();
    let tool_intent_count =
        codeg_lib::db::entities::delegation_completion_tool_intent::Entity::find()
            .filter(
                codeg_lib::db::entities::delegation_completion_tool_intent::Column::TaskId
                    .eq(task_id.clone()),
            )
            .count(&db.conn)
            .await
            .unwrap();

    let direct = project_workflow_graph_core(&state.db, parent)
        .await
        .unwrap();
    let node_index = direct
        .nodes
        .iter()
        .position(|node| node.latest_task_id.as_deref() == Some(task_id.as_str()))
        .expect("capability node projection");
    let desktop = direct.nodes[node_index]
        .completion
        .clone()
        .expect("desktop completion projection");
    let response = server
        .post("/api/get_workflow_graph_snapshot")
        .add_header("authorization", "Bearer task-18-token")
        .json(&json!({ "conversationId": parent }))
        .await;
    response.assert_status_ok();
    let http: Value = response.json();
    assert_eq!(http, serde_json::to_value(&direct).unwrap());
    let server_completion = http["nodes"][node_index]["completion"]["card"].clone();
    let rendered = call_companion_tool(
        &root_companion,
        9,
        "get_delegation_status",
        json!({ "task_ids": [task_id] }),
        Some("task-18-capability-status"),
    )
    .await;
    let mcp_completion = rendered["structuredContent"]["tasks"][0]["completion"]["card"].clone();
    let desktop_completion = serde_json::to_value(&desktop.card).unwrap();
    listener_task.abort();

    CapabilityResult {
        child_run_count: runs.len() as u64,
        card_summary_json: stored_run.card_summary_json,
        completion: desktop.card,
        desktop_completion,
        server_completion,
        mcp_completion,
        tool_intent_count,
    }
}

#[tokio::test]
async fn completion_v2_semantic_inputs() {
    for case in [
        CapabilityCase::ToolCompleteWork,
        CapabilityCase::TerminalConclusionOnly,
        CapabilityCase::ReportConclusionOnly,
        CapabilityCase::AmbiguousThenUserAdjudication,
        CapabilityCase::ObsoleteCardPlusNaturalConclusion,
    ] {
        let result = run_capability_case(case).await;
        assert_eq!(result.child_run_count, 1, "{case:?}");
        assert!(result.card_summary_json.is_none(), "{case:?}");
        assert!(result.completion.evidence_validated, "{case:?}");
        assert_eq!(
            result.desktop_completion, result.server_completion,
            "{case:?}"
        );
        assert_eq!(result.server_completion, result.mcp_completion, "{case:?}");
        assert_eq!(
            result.tool_intent_count,
            u64::from(matches!(case, CapabilityCase::ToolCompleteWork)),
            "{case:?}"
        );
    }
}

fn git_fixture(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("run Task 18 git fixture command");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output is UTF-8")
        .trim()
        .to_string()
}

fn final_review_skeleton(token: &str) -> ManifestDocument {
    let mut document = skeleton(token);
    document.phases.push(ManifestPhase {
        id: PHASE_FINAL.into(),
        kind: Some(PHASE_FINAL.into()),
        title: None,
    });
    document.nodes.push(ManifestNode {
        id: "final-reviewer".into(),
        kind: ManifestNodeKind::WorkUnit,
        phase_id: Some(PHASE_FINAL.into()),
        role: Some(ManifestNodeRole::Reviewer),
        agent_type: Some("codex".into()),
        profile_id: None,
        task_index: None,
        work_unit_key: Some(
            build_work_unit_key(&WorkUnitKeyParts::FinalReviewer {
                agent_type: "codex",
                profile_id: None,
            })
            .unwrap(),
        ),
        deps: Vec::new(),
        required: Some(true),
        node_outcome: None,
        title: None,
    });
    document.nodes.push(ManifestNode {
        id: "final-reviewer-grok".into(),
        kind: ManifestNodeKind::WorkUnit,
        phase_id: Some(PHASE_FINAL.into()),
        role: Some(ManifestNodeRole::Reviewer),
        agent_type: Some("grok".into()),
        profile_id: None,
        task_index: None,
        work_unit_key: Some(
            build_work_unit_key(&WorkUnitKeyParts::FinalReviewer {
                agent_type: "grok",
                profile_id: None,
            })
            .unwrap(),
        ),
        deps: Vec::new(),
        required: Some(true),
        node_outcome: None,
        title: None,
    });
    document.nodes.push(ManifestNode {
        id: "plan-reviewer".into(),
        kind: ManifestNodeKind::WorkUnit,
        phase_id: Some(PHASE_PLAN.into()),
        role: Some(ManifestNodeRole::Reviewer),
        agent_type: Some("codex".into()),
        profile_id: None,
        task_index: None,
        work_unit_key: Some(
            build_work_unit_key(&WorkUnitKeyParts::PlanReviewer {
                rel_plan_path: "docs/superpowers/plans/restarted-plan.md",
                agent_type: "codex",
                profile_id: None,
            })
            .unwrap(),
        ),
        deps: vec!["plan-author".into()],
        required: Some(true),
        node_outcome: None,
        title: None,
    });
    document.gates.push(ManifestGate {
        id: "plan".into(),
        reviewer_cohort_node_ids: vec!["plan-reviewer".into()],
        required_reviewer_node_ids: vec!["plan-reviewer".into()],
        resolution_mode: ResolutionMode::ParentAdjudication,
        gate_kind: Some(DocumentGateKind::Plan),
    });
    document
}

fn session_2889_skeleton(token: &str) -> ManifestDocument {
    let mut document = skeleton(token);
    for (node_id, agent_type) in [
        ("plan-reviewer-codex", "codex"),
        ("plan-reviewer-grok", "grok"),
    ] {
        document.nodes.push(ManifestNode {
            id: node_id.into(),
            kind: ManifestNodeKind::WorkUnit,
            phase_id: Some(PHASE_PLAN.into()),
            role: Some(ManifestNodeRole::Reviewer),
            agent_type: Some(agent_type.into()),
            profile_id: None,
            task_index: None,
            work_unit_key: Some(
                build_work_unit_key(&WorkUnitKeyParts::PlanReviewer {
                    rel_plan_path: "docs/superpowers/plans/restarted-plan.md",
                    agent_type,
                    profile_id: None,
                })
                .unwrap(),
            ),
            deps: vec!["plan-author".into()],
            required: Some(true),
            node_outcome: None,
            title: None,
        });
    }
    document.gates.push(ManifestGate {
        id: "plan".into(),
        reviewer_cohort_node_ids: vec!["plan-reviewer-codex".into(), "plan-reviewer-grok".into()],
        required_reviewer_node_ids: vec!["plan-reviewer-codex".into(), "plan-reviewer-grok".into()],
        resolution_mode: ResolutionMode::ParentAdjudication,
        gate_kind: Some(DocumentGateKind::Plan),
    });
    document
}

struct Session2889Result {
    format_repair_run_count: usize,
    card_reemit_prompt_count: usize,
    plan_reviewer_run_count: usize,
    continuation_error_code: Option<String>,
    resume_call_count: usize,
    spawn_call_count: usize,
}

async fn run_session_2889_fixture() -> Session2889Result {
    const DESIGN_REL_PATH: &str = "docs/superpowers/specs/session-2889-design.md";
    const PLAN_REL_PATH: &str = "docs/superpowers/plans/restarted-plan.md";
    const DESIGN_BYTES: &[u8] = b"# Design\n\nSession 2889 completion regression.\n";
    const PLAN_BYTES: &[u8] = b"# Plan\n\nSession 2889 Plan review regression.\n";

    let workspace = tempfile::tempdir().expect("session 2889 workspace");
    let design_path = workspace.path().join(DESIGN_REL_PATH);
    std::fs::create_dir_all(design_path.parent().unwrap()).unwrap();
    std::fs::write(&design_path, DESIGN_BYTES).unwrap();
    let plan_path = workspace.path().join(PLAN_REL_PATH);
    std::fs::create_dir_all(plan_path.parent().unwrap()).unwrap();
    std::fs::write(&plan_path, PLAN_BYTES).unwrap();

    let db = fresh_in_memory_db().await;
    let folder = seed_folder(&db, workspace.path().to_str().unwrap()).await;
    let parent = seed_conversation(&db, folder, AgentType::Codex).await;
    let mut document = session_2889_skeleton("task-18-session-2889");
    document.workflow_state = ManifestWorkflowState::Estimated;
    document.design = Some(DocumentRef {
        rel_path: DESIGN_REL_PATH.into(),
        digest: format!("sha256:{:x}", Sha256::digest(DESIGN_BYTES)),
    });
    document.plan = Some(DocumentRef {
        rel_path: PLAN_REL_PATH.into(),
        digest: format!("sha256:{:x}", Sha256::digest(PLAN_BYTES)),
    });
    let published = publish_workflow_manifest_fixture(
        &db,
        &EventEmitter::Noop,
        parent,
        PublishWorkflowRequest { document },
    )
    .await
    .unwrap();
    let workflow = delegation_workflow::Entity::find_by_id(&published.workflow_id)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(workflow.completion_protocol_version, 2);
    assert_eq!(
        workflow.completion_protocol_mode,
        delegation_workflow::CompletionProtocolMode::V2Enforce
    );

    let author_task_id = "session-2889-plan-author";
    admit_v2_fixture_run(
        &db,
        parent,
        seed_conversation(&db, folder, AgentType::Codex).await,
        workspace.path(),
        author_task_id,
        "codex",
        build_work_unit_key(&WorkUnitKeyParts::PlanAuthor {
            rel_plan_path: PLAN_REL_PATH,
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap(),
        "Session 2889 Plan Author",
    )
    .await;
    materialize_v2_fixture_run(&db, author_task_id, "Plan authored.\n\nConclusion: done").await;

    let first_reviewer_task_id = "session-2889-plan-reviewer-codex";
    admit_v2_fixture_run(
        &db,
        parent,
        seed_conversation(&db, folder, AgentType::Codex).await,
        workspace.path(),
        first_reviewer_task_id,
        "codex",
        build_work_unit_key(&WorkUnitKeyParts::PlanReviewer {
            rel_plan_path: PLAN_REL_PATH,
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap(),
        "Session 2889 first Plan Reviewer",
    )
    .await;
    materialize_v2_fixture_run(
        &db,
        first_reviewer_task_id,
        "<!-- codeg-card-summary-v1 {\"kind\":\"review\",\"verdict\":\"approve\",\"plan_digest\":\"obsolete-model-value\"} -->\n\nConclusion: approve",
    )
    .await;
    let first_reviewer = delegation_task_run::Entity::find_by_id(first_reviewer_task_id)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    assert!(first_reviewer.card_summary_json.is_none());

    let second_reviewer_task_id = "session-2889-plan-reviewer-grok";
    admit_v2_fixture_run(
        &db,
        parent,
        seed_conversation(&db, folder, AgentType::Grok).await,
        workspace.path(),
        second_reviewer_task_id,
        "grok",
        build_work_unit_key(&WorkUnitKeyParts::PlanReviewer {
            rel_plan_path: PLAN_REL_PATH,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap(),
        "Session 2889 next Plan Reviewer",
    )
    .await;

    let runs = Arc::new(RunStore::new(Arc::new(codeg_lib::db::AppDatabase {
        conn: db.conn.clone(),
    })));
    let spawner = Arc::new(MockSpawner::new());
    spawner
        .queue_spawn(Ok("session-2889-unexpected-spawn".into()))
        .await;
    spawner
        .queue_resume(Ok("session-2889-unexpected-resume".into()))
        .await;
    spawner
        .queue_send(Err(SpawnerError::send(
            "session-2889 Card re-emit prompt must not be sent",
        )))
        .await;
    let broker = DelegationBroker::new(
        Arc::clone(&spawner) as Arc<dyn ConnectionSpawner>,
        Arc::new(RootDepth) as Arc<dyn ConversationDepthLookup>,
    )
    .with_run_store(runs);
    let before_runs = delegation_task_run::Entity::find()
        .filter(delegation_task_run::Column::ParentConversationId.eq(parent))
        .count(&db.conn)
        .await
        .unwrap();
    let continuation = with_historical_workflow_fixture_mutations(broker.continue_delegation(
        ContinueDelegationRequest {
            parent_connection_id: "session-2889-parent".into(),
            parent_conversation_id: parent,
            parent_tool_use_id: "session-2889-card-reemit".into(),
            target_task_id: first_reviewer_task_id.into(),
            task: "CARD RE-EMIT ONLY".into(),
            work_unit_key: first_reviewer.work_unit_key.clone(),
            external_handle: None,
            correlation_id: None,
            recovery_authorization_id: None,
            orchestration_binding: None,
        },
    ))
    .await;
    let persisted_runs = delegation_task_run::Entity::find()
        .filter(delegation_task_run::Column::ParentConversationId.eq(parent))
        .all(&db.conn)
        .await
        .unwrap();
    let after_runs = persisted_runs.len() as u64;
    let plan_reviewer_run_count = persisted_runs
        .iter()
        .filter(|run| {
            run.work_unit_key
                .as_deref()
                .is_some_and(|key| key.contains("|reviewer|"))
        })
        .count();
    let format_repair_run_count = after_runs.saturating_sub(before_runs) as usize;
    let card_reemit_prompt_count = 1usize.saturating_sub(spawner.send_results.lock().await.len());
    let resume_call_count = spawner.resume_args.lock().await.len();
    let spawn_call_count = spawner.spawn_args.lock().await.len();
    Session2889Result {
        format_repair_run_count,
        card_reemit_prompt_count,
        plan_reviewer_run_count,
        continuation_error_code: continuation.error_code,
        resume_call_count,
        spawn_call_count,
    }
}

struct FinalDeliveryResult {
    response: Value,
    reopen_signal: Value,
    reopened: Value,
    gate_id: String,
    review_round: i64,
    graph_has_no_stale_final_completion: bool,
}

#[derive(Clone, Copy)]
enum FinalEnrichmentSurface {
    Report,
    Status,
}

#[derive(Clone, Copy)]
enum FinalArtifactMutation {
    CommitDrift,
    DirtyWorktree,
}

async fn run_final_delivery_fixture(
    surface: FinalEnrichmentSurface,
    mutation: FinalArtifactMutation,
) -> FinalDeliveryResult {
    let repo = tempfile::tempdir().expect("Task 18 final repo");
    git_fixture(repo.path(), &["init", "--quiet"]);
    std::fs::write(repo.path().join("verified.txt"), b"reviewed\n").unwrap();
    git_fixture(repo.path(), &["add", "verified.txt"]);
    git_fixture(
        repo.path(),
        &[
            "-c",
            "user.name=Codeg Test",
            "-c",
            "user.email=codeg@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "reviewed",
        ],
    );
    let db = fresh_in_memory_db().await;
    let folder = seed_folder(&db, repo.path().to_str().unwrap()).await;
    let parent = seed_conversation(&db, folder, AgentType::Codex).await;
    let child = seed_conversation(&db, folder, AgentType::Codex).await;
    let mut document = final_review_skeleton("task-18-final-drift");
    let design_bytes = b"# Design\n\nTask 18 Final delivery requirements.\n";
    let design_path = repo
        .path()
        .join("docs/superpowers/specs/task-18-final-design.md");
    std::fs::create_dir_all(design_path.parent().unwrap()).unwrap();
    std::fs::write(&design_path, design_bytes).unwrap();
    let plan_bytes = b"## Global Constraints\n\n- Task 18 Final delivery fixture.\n";
    let plan_path = repo.path().join("docs/superpowers/plans/restarted-plan.md");
    std::fs::create_dir_all(plan_path.parent().unwrap()).unwrap();
    std::fs::write(&plan_path, plan_bytes).unwrap();
    git_fixture(
        repo.path(),
        &[
            "add",
            "docs/superpowers/specs/task-18-final-design.md",
            "docs/superpowers/plans/restarted-plan.md",
        ],
    );
    git_fixture(
        repo.path(),
        &[
            "-c",
            "user.name=Codeg Test",
            "-c",
            "user.email=codeg@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "add plan",
        ],
    );
    document.workflow_state = ManifestWorkflowState::Estimated;
    document.design = Some(DocumentRef {
        rel_path: "docs/superpowers/specs/task-18-final-design.md".into(),
        digest: format!("sha256:{:x}", Sha256::digest(design_bytes)),
    });
    document.plan = Some(DocumentRef {
        rel_path: "docs/superpowers/plans/restarted-plan.md".into(),
        digest: format!("sha256:{:x}", Sha256::digest(plan_bytes)),
    });
    let published = publish_workflow_manifest_fixture(
        &db,
        &EventEmitter::Noop,
        parent,
        PublishWorkflowRequest { document },
    )
    .await
    .unwrap();

    let plan_author_task_id = "task-18-final-plan-author";
    admit_v2_fixture_run(
        &db,
        parent,
        seed_conversation(&db, folder, AgentType::Codex).await,
        repo.path(),
        plan_author_task_id,
        "codex",
        build_work_unit_key(&WorkUnitKeyParts::PlanAuthor {
            rel_plan_path: "docs/superpowers/plans/restarted-plan.md",
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap(),
        "Task 18 Final Plan Author",
    )
    .await;
    materialize_v2_fixture_run(
        &db,
        plan_author_task_id,
        "Plan authored.\n\nConclusion: done",
    )
    .await;
    let plan_reviewer_task_id = "task-18-final-plan-reviewer";
    admit_v2_fixture_run(
        &db,
        parent,
        seed_conversation(&db, folder, AgentType::Codex).await,
        repo.path(),
        plan_reviewer_task_id,
        "codex",
        build_work_unit_key(&WorkUnitKeyParts::PlanReviewer {
            rel_plan_path: "docs/superpowers/plans/restarted-plan.md",
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap(),
        "Task 18 Final Plan Reviewer",
    )
    .await;
    materialize_v2_fixture_run(
        &db,
        plan_reviewer_task_id,
        "Plan review passed.\n\nConclusion: approve",
    )
    .await;
    let current = delegation_workflow::Entity::find_by_id(&published.workflow_id)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    let settled = settle_workflow_gate_v2_core(
        &db,
        &EventEmitter::Noop,
        parent,
        SettleWorkflowV2Request {
            workflow_id: published.workflow_id.clone(),
            gate_id: "plan".into(),
            expected_graph_revision: current.graph_revision as u64,
            expected_review_round: Some(1),
            expected_outcome: Some(GateSettlementOutcome::Approved),
            summary: "Task 18 Final prerequisite Plan approval".into(),
            recovery_authorization_id: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(settled.outcome, GateSettlementOutcome::Approved);
    let final_state = delegation_workflow_gate_state::Entity::find_by_id((
        published.workflow_id.clone(),
        "final".to_string(),
    ))
    .one(&db.conn)
    .await
    .unwrap()
    .unwrap();
    let mut final_state: delegation_workflow_gate_state::ActiveModel = final_state.into();
    final_state.selected_node_ids_json = Set("[\"final-reviewer-grok\",\"final-reviewer\"]".into());
    final_state.update(&db.conn).await.unwrap();

    let task_id = "task-18-passing-final-review";
    admit_v2_fixture_run(
        &db,
        parent,
        child,
        repo.path(),
        task_id,
        "codex",
        build_work_unit_key(&WorkUnitKeyParts::FinalReviewer {
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap(),
        "Task 18 Final review",
    )
    .await;
    let unvalidated_run = delegation_task_run::Entity::find_by_id(task_id)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    let mut unvalidated_run: delegation_task_run::ActiveModel = unvalidated_run.into();
    unvalidated_run.status = Set(delegation_task_run::DelegationRunStatus::Completed);
    unvalidated_run.reached_running_at = Set(Some(chrono::Utc::now()));
    unvalidated_run.finished_at = Set(Some(chrono::Utc::now()));
    unvalidated_run.update(&db.conn).await.unwrap();
    let unvalidated_delivery = guard_final_delivery_core(
        &db,
        &EventEmitter::Noop,
        FinalDeliveryGuardRequest {
            workflow_id: published.workflow_id.clone(),
            gate_id: "final".into(),
            workspace_path: repo.path().to_path_buf(),
            final_reviewer_task_id: task_id.into(),
        },
    )
    .await;
    assert!(
        unvalidated_delivery.is_err(),
        "a merely completed Final run without validated v2 evidence must not be deliverable"
    );
    materialize_v2_fixture_run(
        &db,
        task_id,
        "Final review complete.\n\nConclusion: approve",
    )
    .await;
    let grok_task_id = "task-18-passing-final-review-grok";
    admit_v2_fixture_run(
        &db,
        parent,
        seed_conversation(&db, folder, AgentType::Grok).await,
        repo.path(),
        grok_task_id,
        "grok",
        build_work_unit_key(&WorkUnitKeyParts::FinalReviewer {
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap(),
        "Task 18 Grok Final review",
    )
    .await;
    materialize_v2_fixture_run(
        &db,
        grok_task_id,
        "Independent Final review complete.\n\nConclusion: approve",
    )
    .await;
    let ready = guard_final_delivery_core(
        &db,
        &EventEmitter::Noop,
        FinalDeliveryGuardRequest {
            workflow_id: published.workflow_id.clone(),
            gate_id: "final".into(),
            workspace_path: repo.path().to_path_buf(),
            final_reviewer_task_id: task_id.into(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(ready, FinalDeliveryGuardResult::Ready(_)));

    std::fs::write(repo.path().join("verified.txt"), b"post-settlement drift\n").unwrap();
    if matches!(mutation, FinalArtifactMutation::CommitDrift) {
        git_fixture(repo.path(), &["add", "verified.txt"]);
        git_fixture(
            repo.path(),
            &[
                "-c",
                "user.name=Codeg Test",
                "-c",
                "user.email=codeg@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "post-settlement drift",
            ],
        );
    }

    let runs = Arc::new(RunStore::new(Arc::new(codeg_lib::db::AppDatabase {
        conn: db.conn.clone(),
    })));
    let broker = Arc::new(
        DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            Arc::new(RootDepth) as Arc<dyn ConversationDepthLookup>,
        )
        .with_run_store(runs),
    );
    let tokens = Arc::new(TokenRegistry::default());
    let root_token = "task-18-final-root";
    tokens
        .register(
            root_token.into(),
            TokenEntry {
                parent_connection_id: "task-18-final-parent".into(),
                working_dir: repo.path().to_path_buf(),
                coordination_v1: false,
                delegation_continuation_v1: false,
                role: CompanionRole::Root,
                workflow_v2: true,
                completion_v2: false,
                bound_task_id: None,
            },
        )
        .await;
    let listener = DelegationListener::new_with_workflow_emitter(
        broker,
        tokens,
        Arc::new(CompanionLeaseRegistry::default()),
        Arc::new(FixedParent(parent)) as Arc<dyn ParentSessionLookup>,
        Arc::new(NoFeedback),
        Arc::new(NoQuestions),
        Arc::new(NoSessionInfo),
        codeg_lib::acp::delegation::wait_cancel::WaitCancelRegistry::new_shared(),
        EventEmitter::Noop,
    );
    let socket_path = workflow_socket_path();
    let listener_task = tokio::spawn(with_historical_workflow_fixture_mutations(
        listener.run(socket_path.clone()),
    ));
    // Retry only transport readiness (NotFound / ConnectionRefused). Do not
    // pre-probe with get_workflow_state: that path mutates delivery state and
    // can change the first enrichment error_code under test.
    let response = match surface {
        FinalEnrichmentSurface::Report => {
            client_cancel_task_when_ready(
                socket_path.to_string_lossy().as_ref(),
                &BrokerCancelTaskRequest {
                    token: root_token.into(),
                    task_id: task_id.into(),
                    reason: CancelDelegationReason::TaskFail,
                },
            )
            .await
            .expect("cancel after listener bind")
            .outcome
        }
        FinalEnrichmentSurface::Status => {
            client_status_when_ready(
                socket_path.to_string_lossy().as_ref(),
                &BrokerStatusRequest {
                    token: root_token.into(),
                    task_ids: vec![task_id.into(), plan_author_task_id.into()],
                    wait_ms: None,
                    return_when: None,
                    parent_tool_use_id: "task-18-final-status".into(),
                },
            )
            .await
            .expect("status after listener bind")
            .outcome
        }
    };
    let (reopen_signal, reopened) = if matches!(mutation, FinalArtifactMutation::CommitDrift) {
        (
            wait_for_workflow_listener(&socket_path, root_token, &published.workflow_id).await,
            wait_for_workflow_listener(&socket_path, root_token, &published.workflow_id).await,
        )
    } else {
        (Value::Null, Value::Null)
    };
    listener_task.abort();
    let graph = project_workflow_graph_core(&db, parent).await;
    let graph_has_no_stale_final_completion = graph.as_ref().is_some_and(|graph| {
        let final_reviewers = graph.nodes.iter().filter(|node| {
            node.phase_id.as_deref() == Some(PHASE_FINAL)
                && node.role.as_deref() == Some("reviewer")
        });
        let final_reviewers = final_reviewers.collect::<Vec<_>>();
        final_reviewers.len() == 2 && final_reviewers.iter().all(|node| node.completion.is_none())
    });
    let gate = delegation_workflow_gate_state::Entity::find_by_id((
        published.workflow_id,
        "final".to_string(),
    ))
    .one(&db.conn)
    .await
    .unwrap()
    .unwrap();
    FinalDeliveryResult {
        response,
        reopen_signal,
        reopened,
        gate_id: gate.gate_id,
        review_round: gate.current_review_round,
        graph_has_no_stale_final_completion,
    }
}

#[tokio::test]
async fn session_2889_and_final_drift_have_no_format_repair_escape() {
    let session = run_session_2889_fixture().await;
    assert_eq!(session.format_repair_run_count, 0);
    assert_eq!(session.card_reemit_prompt_count, 0);
    assert_eq!(session.plan_reviewer_run_count, 2);
    assert_eq!(
        session.continuation_error_code.as_deref(),
        Some("completion_card_reemit_forbidden")
    );
    assert_eq!(session.resume_call_count, 0);
    assert_eq!(session.spawn_call_count, 0);

    let final_drift = run_final_delivery_fixture(
        FinalEnrichmentSurface::Status,
        FinalArtifactMutation::CommitDrift,
    )
    .await;
    let drifted_final = &final_drift.response["tasks"][0];
    assert_eq!(
        drifted_final.get("error_code").and_then(Value::as_str),
        Some("final_artifact_drift")
    );
    assert_eq!(drifted_final["status"], "failed");
    assert!(drifted_final.get("text").is_none());
    assert!(drifted_final.get("completion").is_none());
    assert_ne!(
        final_drift.response["tasks"][1]
            .get("error_code")
            .and_then(Value::as_str),
        Some("final_artifact_drift"),
        "non-Final status enrichment must not be rewritten by the delivery guard"
    );
    assert!(
        final_drift.reopen_signal.get("error").is_none(),
        "status enrichment must reopen Final before workflow-state projection: {}",
        final_drift.reopen_signal
    );
    assert!(final_drift.reopened.get("error").is_none());
    assert_eq!(final_drift.reopened["workflow_state"], "approved");
    assert_eq!(final_drift.reopened["detail"], "index");
    assert_eq!(final_drift.gate_id, "final");
    assert_eq!(final_drift.review_round, 2);
}

#[tokio::test]
async fn reopened_final_projection_omits_every_stale_reviewer_completion() {
    let final_drift = run_final_delivery_fixture(
        FinalEnrichmentSurface::Status,
        FinalArtifactMutation::CommitDrift,
    )
    .await;
    assert!(
        final_drift.graph_has_no_stale_final_completion,
        "both stale Final reviewer completions must be absent after drift"
    );
}

#[tokio::test]
async fn final_drift_report_enrichment_reopens_and_omits_stale_completion() {
    let final_drift = run_final_delivery_fixture(
        FinalEnrichmentSurface::Report,
        FinalArtifactMutation::CommitDrift,
    )
    .await;
    assert_eq!(final_drift.response["status"], "failed");
    assert_eq!(final_drift.response["error_code"], "final_artifact_drift");
    assert!(final_drift.response.get("text").is_none());
    assert!(final_drift.response.get("completion").is_none());
    assert_eq!(final_drift.review_round, 2);
}

#[tokio::test]
async fn final_status_enrichment_preserves_artifact_unavailable_diagnostic() {
    let unavailable = run_final_delivery_fixture(
        FinalEnrichmentSurface::Status,
        FinalArtifactMutation::DirtyWorktree,
    )
    .await;
    let rejected_final = &unavailable.response["tasks"][0];
    assert_eq!(rejected_final["status"], "failed");
    assert_eq!(
        rejected_final["error_code"],
        "completion_artifact_unavailable"
    );
    assert!(rejected_final.get("text").is_none());
    assert!(rejected_final.get("completion").is_none());
    assert_eq!(unavailable.review_round, 1);
    assert!(unavailable.reopen_signal.is_null());
    assert!(unavailable.reopened.is_null());
}

#[tokio::test]
async fn historical_protocol_projection() {
    use delegation_workflow::CompletionProtocolMode::{V2Shadow, V1};

    const CARD_JSON: &str = r#"{ "kind":"implementation", "phase":"implementation", "status":"done", "summary":"historical card bytes", "commits":[], "concerns":[] }"#;

    for (index, persisted_mode) in [V1, V2Shadow].into_iter().enumerate() {
        let source_parent = 101;
        let successor_parent = 102;
        let source_workflow_id = format!("wf-task-6-historical-source-{index}");
        let successor_workflow_id = format!("wf-task-6-historical-successor-{index}");
        let db = historical_completion_protocol_db(&[
            historical_workflow_seed(
                &source_workflow_id,
                source_parent,
                1,
                persisted_mode.clone(),
                None,
            ),
            historical_workflow_seed(
                &successor_workflow_id,
                successor_parent,
                2,
                delegation_workflow::CompletionProtocolMode::V2Enforce,
                Some(source_workflow_id.clone()),
            ),
        ])
        .await;
        let child = seed_conversation(&db, 1, AgentType::Codex).await;
        let source = seed_historical_manifest(
            &db,
            &source_workflow_id,
            &skeleton(&format!("task-6-historical-source-{index}")),
        )
        .await;
        let successor = seed_historical_manifest(
            &db,
            &successor_workflow_id,
            &skeleton(&format!("task-6-historical-successor-{index}")),
        )
        .await;
        let context_request_id = format!("historical-request-{index}");
        delegation_workflow_restart_context::ActiveModel {
            conversation_id: Set(source_parent),
            original_conversation_id: Set(source_parent),
            original_request_id: Set(context_request_id.clone()),
            original_request_text: Set("preserved historical request".into()),
            original_request_digest: Set(format!("sha256:{}", "a".repeat(64))),
            agent_type: Set("codex".into()),
            profile_id: Set(Some("historical-profile".into())),
            created_at: Set(chrono::Utc::now()),
        }
        .insert(&db.conn)
        .await
        .unwrap();

        let task_id = format!("task-6-historical-card-{index}");
        seed_conversation_workflow_association(
            &db,
            source_parent,
            child,
            &task_id,
            1,
            chrono::Utc::now(),
            Some(&source.workflow_id),
        )
        .await;
        let run = delegation_task_run::Entity::find_by_id(&task_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut run: delegation_task_run::ActiveModel = run.into();
        run.card_summary_json = Set(Some(CARD_JSON.to_string()));
        run.update(&db.conn).await.unwrap();
        let binding = delegation_workflow_run_binding::Entity::find_by_id(&task_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut binding: delegation_workflow_run_binding::ActiveModel = binding.into();
        binding.summary_validated = Set(true);
        binding.update(&db.conn).await.unwrap();

        let source_graph = project_workflow_graph_core(&db, source_parent)
            .await
            .unwrap();
        let projection = source_graph.completion_protocol.unwrap();
        assert_eq!(projection.version, 1);
        assert_eq!(projection.mode, persisted_mode);
        assert_eq!(projection.creation_mode, persisted_mode);
        assert_eq!(
            projection.read_only_reason.as_deref(),
            Some("legacy_completion_protocol_read_only")
        );
        assert!(!projection.automatic_root_wake);
        assert!(projection.legacy_source.is_none());
        let successor_link = projection.v2_successor.expect("historical successor link");
        assert_eq!(successor_link.workflow_id, successor.workflow_id);
        assert_eq!(successor_link.conversation_id, successor_parent);

        let card_node = source_graph
            .nodes
            .iter()
            .find(|node| node.latest_task_id.as_deref() == Some(task_id.as_str()))
            .expect("historical Card node");
        assert_eq!(card_node.summary.as_deref(), Some("historical card bytes"));
        let stored_card = delegation_task_run::Entity::find_by_id(&task_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap()
            .card_summary_json;
        assert_eq!(stored_card.as_deref(), Some(CARD_JSON));
        let stored_context = load_historical_workflow_context(&db.conn, source_parent)
            .await
            .unwrap()
            .expect("historical context row");
        assert_eq!(stored_context.original_request_id, context_request_id);
        assert_eq!(
            stored_context.original_request_text,
            "preserved historical request"
        );

        let successor_graph = project_workflow_graph_core(&db, successor_parent)
            .await
            .unwrap();
        let successor_projection = successor_graph.completion_protocol.unwrap();
        let source_link = successor_projection
            .legacy_source
            .expect("historical source link");
        assert_eq!(source_link.workflow_id, source.workflow_id);
        assert_eq!(source_link.conversation_id, source_parent);
        assert!(successor_projection.v2_successor.is_none());
        assert_eq!(
            successor_projection.creation_mode,
            successor_projection.mode
        );
    }
}

async fn source_fingerprint(
    db: &codeg_lib::db::AppDatabase,
    parent: i32,
    workflow_id: &str,
) -> String {
    let workflow = delegation_workflow::Entity::find_by_id(workflow_id)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    let conversation = codeg_lib::db::entities::conversation::Entity::find_by_id(parent)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    let revisions = delegation_workflow_manifest_revision::Entity::find()
        .filter(delegation_workflow_manifest_revision::Column::WorkflowId.eq(workflow_id))
        .count(&db.conn)
        .await
        .unwrap();
    format!("{workflow:?}|{conversation:?}|{revisions}")
}

#[tokio::test]
async fn root_prompt_protocol_fence() {
    use delegation_workflow::CompletionProtocolMode::{V2Enforce, V2Shadow, V1};

    for (index, version, mode, expected_code) in [
        (0, 1, V1, "legacy_completion_protocol_read_only"),
        (1, 1, V2Shadow, "legacy_completion_protocol_read_only"),
        (2, 1, V2Enforce, "legacy_completion_protocol_read_only"),
        (3, 2, V1, "unsupported_completion_protocol"),
        (4, 2, V2Shadow, "unsupported_completion_protocol"),
    ] {
        let folder_id = 1;
        let parent = 101;
        let workflow_id = format!("wf-task-4-root-fence-{index}");
        let db = historical_completion_protocol_db(&[historical_workflow_seed(
            &workflow_id,
            parent,
            version,
            mode,
            None,
        )])
        .await;
        let published = seed_historical_manifest(
            &db,
            &workflow_id,
            &skeleton(&format!("task-4-root-fence-{index}")),
        )
        .await;
        let before = mutation_snapshot(&db, parent, &published.workflow_id).await;

        let manager = ConnectionManager::new();
        let connection_id = format!("task-4-root-{index}");
        manager
            .insert_test_connection(
                &connection_id,
                AgentType::Codex,
                Some(std::path::PathBuf::from(format!(
                    "/tmp/task-4-root-fence-{index}"
                ))),
                EventEmitter::Noop,
            )
            .await;
        let state = manager.get_state(&connection_id).await.unwrap();
        state.write().await.conversation_id = Some(parent);

        let foreground = manager
            .send_prompt_linked_with_message_id(
                &db,
                &connection_id,
                vec![PromptInputBlock::Text {
                    text: "foreground resume must be rejected".into(),
                }],
                Some(folder_id),
                Some(parent),
                None,
                Some(format!("task-4-foreground-{index}")),
                None,
            )
            .await
            .expect_err("linked foreground prompt must fail before admission");
        assert_eq!(foreground.code(), Some(expected_code));

        let background = manager
            .send_prompt_linked_background(
                &db,
                &connection_id,
                vec![PromptInputBlock::Text {
                    text: "automation and chat resume must be rejected".into(),
                }],
                Some(folder_id),
                Some(parent),
                None,
            )
            .await
            .expect_err("linked background prompt must fail before admission");
        assert_eq!(background.code(), Some(expected_code));

        assert!(!state.read().await.turn_in_flight);
        assert_eq!(
            mutation_snapshot(&db, parent, &published.workflow_id).await,
            before,
            "root admission rejection must not create successors or mutate state"
        );
    }
}

#[tokio::test]
async fn root_protocol_loader_scans_older_bound_generation_when_latest_is_unbound() {
    let workflow_parent = 101;
    let workflow_id = "wf-task-4-root-multi-generation";
    let db = historical_completion_protocol_db(&[historical_workflow_seed(
        workflow_id,
        workflow_parent,
        1,
        delegation_workflow::CompletionProtocolMode::V1,
        None,
    )])
    .await;
    let child = seed_conversation(&db, 1, AgentType::Codex).await;
    let published =
        seed_historical_manifest(&db, workflow_id, &skeleton("task-4-root-multi-generation")).await;
    let base = chrono::Utc::now();
    seed_conversation_workflow_association(
        &db,
        workflow_parent,
        child,
        "task-4-root-bound-generation-1",
        1,
        base,
        Some(&published.workflow_id),
    )
    .await;
    seed_conversation_workflow_association(
        &db,
        workflow_parent,
        child,
        "task-4-root-unbound-generation-2",
        2,
        base + chrono::Duration::seconds(1),
        None,
    )
    .await;
    seed_conversation_workflow_association(
        &db,
        workflow_parent,
        child,
        "task-4-root-unbound-generation-3",
        3,
        base + chrono::Duration::seconds(2),
        None,
    )
    .await;

    let first = load_completion_protocol_for_conversation(&db, child)
        .await
        .unwrap();
    assert_eq!(
        first,
        Some((1, delegation_workflow::CompletionProtocolMode::V1,)),
        "newer unbound generations must not mask an older durable workflow binding"
    );
}

#[tokio::test]
async fn root_protocol_loader_retires_when_conversation_owns_archived_v2() {
    let legacy_parent = 101;
    let legacy_workflow_id = "wf-task-4-root-bound-legacy";
    let db = historical_completion_protocol_db(&[historical_workflow_seed(
        legacy_workflow_id,
        legacy_parent,
        1,
        delegation_workflow::CompletionProtocolMode::V1,
        None,
    )])
    .await;
    let child = seed_conversation(&db, 1, AgentType::Codex).await;
    let legacy = seed_historical_manifest(
        &db,
        legacy_workflow_id,
        &skeleton("task-4-root-bound-legacy"),
    )
    .await;
    let owned = publish_workflow_manifest_fixture(
        &db,
        &EventEmitter::Noop,
        child,
        PublishWorkflowRequest {
            document: skeleton("task-4-root-owned-v2"),
        },
    )
    .await
    .unwrap();
    seed_conversation_workflow_association(
        &db,
        legacy_parent,
        child,
        "task-4-root-owned-bound-task",
        1,
        chrono::Utc::now(),
        Some(&legacy.workflow_id),
    )
    .await;

    let first = load_completion_protocol_for_conversation(&db, child)
        .await
        .expect_err("owned archived v2 workflow must retire the loader");
    assert_eq!(
        first.code(),
        "workflow_v2_retired",
        "a bound legacy workflow must not mask the owned archived workflow"
    );
    let second = load_completion_protocol_for_conversation(&db, child)
        .await
        .expect_err("owned archived v2 workflow must retire every loader attempt");
    assert!(matches!(
        first,
        WorkflowStoreError::WorkflowV2Retired { .. }
    ));
    assert!(matches!(
        second,
        WorkflowStoreError::WorkflowV2Retired { .. }
    ));
    assert_eq!(second.code(), first.code());
    assert_ne!(legacy.workflow_id, owned.workflow_id);
}

#[tokio::test]
async fn historical_root_resume_is_rejected_before_side_effects() {
    let folder = 1;
    let parent = 101;
    let workflow_id = "wf-task-15-pre-migration-source";
    let db = historical_completion_protocol_db(&[historical_workflow_seed(
        workflow_id,
        parent,
        1,
        delegation_workflow::CompletionProtocolMode::V1,
        None,
    )])
    .await;
    let original_request = "implement the historical Task 15 request after upgrading";
    let now = chrono::Utc::now();
    auto_title_job::ActiveModel {
        conversation_id: Set(parent),
        state: Set(auto_title_job::AutoTitleJobState::AwaitingTurn),
        attempts: Set(0),
        first_user_text: Set(Some(original_request.into())),
        first_assistant_text: Set(None),
        first_prompt_at: Set(Some(now)),
        locale: Set(Some("en".into())),
        usable_turn_seq: Set(0),
        attempt_turn_seq: Set(0),
        last_usable_turn_token: Set(None),
        config_gen: Set(0),
        updated_at: Set(now),
    }
    .insert(&db.conn)
    .await
    .unwrap();
    let published =
        seed_historical_manifest(&db, workflow_id, &skeleton("task-15-pre-migration-source")).await;
    assert!(
        delegation_workflow_restart_context::Entity::find_by_id(parent)
            .one(&db.conn)
            .await
            .unwrap()
            .is_none()
    );
    let before = source_fingerprint(&db, parent, &published.workflow_id).await;

    let manager = ConnectionManager::new();
    manager
        .insert_test_connection(
            "upgraded-legacy-root",
            AgentType::Codex,
            Some(std::path::PathBuf::from("/tmp/task-15-legacy-upgrade")),
            EventEmitter::Noop,
        )
        .await;
    let state = manager.get_state("upgraded-legacy-root").await.unwrap();
    state.write().await.conversation_id = Some(parent);

    let error = manager
        .send_prompt_linked_with_message_id(
            &db,
            "upgraded-legacy-root",
            vec![PromptInputBlock::Text {
                text: "this resume prompt must not replace the original request".into(),
            }],
            Some(folder),
            Some(parent),
            None,
            Some("upgrade-resume-turn".into()),
            None,
        )
        .await
        .expect_err("historical root resume must be rejected as read-only");
    assert!(matches!(error, AcpError::LegacyCompletionProtocolReadOnly));

    assert_eq!(
        source_fingerprint(&db, parent, &published.workflow_id).await,
        before,
        "root admission rejection must not mutate the source workflow or conversation"
    );
    assert!(!state.read().await.turn_in_flight);
    assert!(
        delegation_workflow_restart_context::Entity::find_by_id(parent)
            .one(&db.conn)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        delegation_workflow::Entity::find()
            .filter(delegation_workflow::Column::LegacySourceWorkflowId.eq(&published.workflow_id))
            .count(&db.conn)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn valid_v2_attention_outbox_replay_wakes_root_once_and_acknowledges_delivery() {
    let db = Arc::new(fresh_in_memory_db().await);
    let folder = seed_folder(&db, "/tmp/task-8-root-wake").await;
    let parent = seed_conversation(&db, folder, AgentType::Codex).await;
    let published = publish_workflow_manifest_fixture(
        &db,
        &EventEmitter::Noop,
        parent,
        PublishWorkflowRequest {
            document: skeleton("task-8-root-wake"),
        },
    )
    .await
    .unwrap();
    let workflow = delegation_workflow::Entity::find_by_id(&published.workflow_id)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(workflow.completion_protocol_version, 2);
    assert_eq!(
        workflow.completion_protocol_mode,
        delegation_workflow::CompletionProtocolMode::V2Enforce
    );

    let event_id = format!("task-8-root-wake-{}", uuid::Uuid::new_v4());
    let payload = CompletionDecisionResolvedPayloadV1 {
        version: 1,
        event_id: event_id.clone(),
        workflow_id: published.workflow_id.clone(),
        task_id: "task-8-completed-child".into(),
        node_id: "plan-author".into(),
        kind: delegation_attention_request::AttentionKind::CompletionDecision,
        outcome: CompletionOutcome::Done,
        evidence_scope_digest: format!("sha256:{}", "a".repeat(64)),
        graph_revision: published.graph_revision,
    };
    delegation_workflow_outbox_event::ActiveModel {
        event_id: Set(event_id.clone()),
        workflow_id: Set(published.workflow_id),
        graph_revision: Set(i64::try_from(published.graph_revision).unwrap()),
        event_kind: Set(COMPLETION_DECISION_RESOLVED_EVENT.into()),
        subject_key: Set(payload.task_id.clone()),
        payload_json: Set(serde_json::to_string(&payload).unwrap()),
        dispatch_attempts: Set(0),
        created_at: Set(chrono::Utc::now()),
        delivered_at: Set(None),
    }
    .insert(&db.conn)
    .await
    .unwrap();

    let wake = Arc::new(RecordingCompletionRootWake::default());
    let dispatcher = CompletionOutboxDispatcher::new(db.clone(), EventEmitter::Noop)
        .with_root_wake(wake.clone());
    dispatcher.dispatch_pending().await.unwrap();

    assert_eq!(wake.event_ids.lock().await.as_slice(), [event_id.as_str()]);
    let delivered = delegation_workflow_outbox_event::Entity::find_by_id(event_id)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(delivered.dispatch_attempts, 1);
    assert!(delivered.delivered_at.is_some());
}

#[test]
fn completion_protocol_metrics_retain_v2_intent_and_outcome_fields() {
    let metrics = DelegationMetrics::default();
    metrics.record_completion_resolution(
        CompletionIntentSource::UserAdjudication,
        CompletionRole::Reviewer,
    );
    assert_eq!(
        metrics
            .snapshot()
            .completion_protocol
            .natural_language_fallback_count,
        0
    );
    metrics.record_completion_resolution(
        CompletionIntentSource::AssistantConclusion,
        CompletionRole::Reviewer,
    );
    let snapshot = metrics.snapshot().completion_protocol;
    assert_eq!(snapshot.resolutions["user_adjudication:reviewer"], 1);
    assert_eq!(snapshot.resolutions["assistant_conclusion:reviewer"], 1);
    assert_eq!(snapshot.format_only_child_runs, 0);
    assert_eq!(snapshot.card_reemit_prompts, 0);
    assert_eq!(snapshot.natural_language_fallback_count, 1);
}

#[test]
fn completion_protocol_v2_rejects_and_counts_card_only_repair_attempts() {
    assert!(is_completion_format_repair_prompt("  CARD RE-EMIT ONLY  "));
    assert!(!is_completion_format_repair_prompt(
        "continue the implementation with these findings"
    ));
    let metrics = DelegationMetrics::default();
    assert!(!metrics
        .record_format_repair_child_run(delegation_workflow::CompletionProtocolMode::V2Enforce,));
    assert!(
        !metrics.record_card_reemit_prompt(delegation_workflow::CompletionProtocolMode::V2Enforce,)
    );
    let snapshot = metrics.snapshot().completion_protocol;
    assert_eq!(snapshot.format_only_child_runs, 1);
    assert_eq!(snapshot.card_reemit_prompts, 1);
}

#[test]
fn completion_protocol_metrics_record_owned_live_transitions() {
    let metrics = DelegationMetrics::default();
    metrics.record_completion_decision_opened();
    metrics.record_completion_decision_resolved(std::time::Duration::from_millis(125), false);
    metrics.record_completion_decision_superseded();
    metrics.record_completion_open_decision_age(std::time::Duration::from_millis(300));
    metrics.record_completion_outbox_pending(2);
    metrics.record_completion_outbox_retry();
    metrics.record_completion_outbox_delivered(std::time::Duration::from_millis(50));
    metrics.record_completion_plan_classification(PlanReviewChangeV2::Corrective, true, false);
    metrics.record_completion_plan_reducer(PlanReviewNextAction::ContinueReview, 1, false);
    metrics.record_completion_final_state(CompletionFinalMetricState::ContextAvailable);
    metrics.record_completion_final_state(CompletionFinalMetricState::PackagePersisted);
    metrics.record_completion_continuation(CompletionContinuationReason::DecisionResolved);
    metrics.record_completion_sibling_reruns(2);

    let snapshot = metrics.snapshot().completion_protocol;
    assert_eq!(snapshot.decision_lifecycle["opened"], 1);
    assert_eq!(snapshot.decision_lifecycle["resolved"], 1);
    assert_eq!(snapshot.decision_lifecycle["superseded"], 1);
    assert_eq!(snapshot.adjudication_latency_ms_count, 1);
    assert_eq!(snapshot.adjudication_latency_ms_total, 125);
    assert_eq!(snapshot.oldest_open_decision_age_ms, 300);
    assert_eq!(snapshot.outbox_states["pending"], 2);
    assert_eq!(snapshot.outbox_states["retry"], 1);
    assert_eq!(snapshot.outbox_states["delivered"], 1);
    assert_eq!(snapshot.outbox_latency_ms_count, 1);
    assert_eq!(snapshot.outbox_latency_ms_total, 50);
    assert_eq!(snapshot.plan_classifications["corrective:intersects"], 1);
    assert_eq!(
        snapshot.plan_reducer_states["continue_review:stagnation_1:no_rewrite"],
        1
    );
    assert_eq!(snapshot.final_context_states["context_available"], 1);
    assert_eq!(snapshot.final_context_states["package_persisted"], 1);
    assert_eq!(snapshot.continuation_reasons["decision_resolved"], 1);
    assert_eq!(snapshot.sibling_reruns, 2);
}

trait ProtocolPairExt {
    fn protocol_pair(
        &self,
    ) -> (
        i64,
        codeg_lib::db::entities::delegation_workflow::CompletionProtocolMode,
    );
}

impl ProtocolPairExt for delegation_workflow::Model {
    fn protocol_pair(
        &self,
    ) -> (
        i64,
        codeg_lib::db::entities::delegation_workflow::CompletionProtocolMode,
    ) {
        (
            self.completion_protocol_version,
            self.completion_protocol_mode.clone(),
        )
    }
}

struct DanglingTerminalCodes {
    row_code: String,
    wait_code: String,
    event_code: String,
}

async fn enable_delegation_for_aggregate(broker: &DelegationBroker) {
    broker
        .set_config(DelegationConfig {
            enabled: true,
            ..DelegationConfig::default()
        })
        .await;
}

async fn aggregate_broker(
    db: Arc<codeg_lib::db::AppDatabase>,
) -> (
    Arc<RunStore>,
    Arc<MockEventEmitter>,
    Arc<DelegationBroker>,
    Arc<MockSpawner>,
) {
    let runs = Arc::new(RunStore::new(db));
    let mock = Arc::new(MockSpawner::new());
    let events = Arc::new(MockEventEmitter::new());
    let task_store = Arc::new(DbDelegationTaskStore::from_run_store(runs.clone()))
        as Arc<dyn DelegationTaskStore>;
    let broker = Arc::new(
        DelegationBroker::with_writers(
            mock.clone() as Arc<dyn ConnectionSpawner>,
            Arc::new(RootDepth) as Arc<dyn ConversationDepthLookup>,
            Arc::new(NoopMetaWriter) as Arc<dyn DelegationMetaWriter>,
            events.clone() as Arc<dyn DelegationEventEmitter>,
        )
        .with_task_store(task_store)
        .with_run_store(runs.clone()),
    );
    enable_delegation_for_aggregate(&broker).await;
    (runs, events, broker, mock)
}

/// Pre-final aggregate acceptance: fixed v2 creation, child binding, one
/// semantic completion, historical v1 projection, rejected v1 mutation,
/// dangling terminal code parity, and standalone Card display.
#[tokio::test]
async fn v2_only_aggregate_acceptance() {
    use delegation_workflow::CompletionProtocolMode;

    let workspace = tempfile::tempdir().expect("aggregate workspace");
    let plan_rel = "docs/superpowers/plans/restarted-plan.md";
    let design_rel = "docs/superpowers/specs/task-10-aggregate-design.md";
    let design_bytes = b"# Design\n\nAggregate v2-only acceptance requirements.\n";
    let plan_path = workspace.path().join(plan_rel);
    let design_path = workspace.path().join(design_rel);
    std::fs::create_dir_all(plan_path.parent().unwrap()).unwrap();
    std::fs::create_dir_all(design_path.parent().unwrap()).unwrap();
    std::fs::write(&plan_path, b"# Plan\n\nAggregate v2-only acceptance.\n").unwrap();
    std::fs::write(&design_path, design_bytes).unwrap();

    let db = fresh_in_memory_db().await;
    let folder = seed_folder(&db, workspace.path().to_str().unwrap()).await;
    let parent = seed_conversation(&db, folder, AgentType::Codex).await;
    let mut live_document = skeleton("task-10-aggregate-v2");
    live_document.design = Some(DocumentRef {
        rel_path: design_rel.into(),
        digest: format!("sha256:{:x}", Sha256::digest(design_bytes)),
    });
    let published = publish_workflow_manifest_fixture(
        &db,
        &EventEmitter::Noop,
        parent,
        PublishWorkflowRequest {
            document: live_document,
        },
    )
    .await
    .unwrap();
    let new_workflow = delegation_workflow::Entity::find_by_id(&published.workflow_id)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        new_workflow.protocol_pair(),
        (2, CompletionProtocolMode::V2Enforce)
    );

    let work_unit_key = build_work_unit_key(&WorkUnitKeyParts::PlanAuthor {
        rel_plan_path: plan_rel,
        agent_type: "codex",
        profile_id: None,
    })
    .unwrap();
    let semantic_task_id = "task-10-aggregate-semantic";
    let child = seed_conversation(&db, folder, AgentType::Codex).await;
    admit_v2_fixture_run(
        &db,
        parent,
        child,
        workspace.path(),
        semantic_task_id,
        "codex",
        work_unit_key.clone(),
        "Task 10 aggregate semantic",
    )
    .await;
    let binding = delegation_workflow_run_binding::Entity::find_by_id(semantic_task_id)
        .one(&db.conn)
        .await
        .unwrap()
        .expect("v2 child binding must exist after admission");
    assert_eq!(binding.workflow_id, published.workflow_id);
    assert_eq!(binding.workflow_id, new_workflow.workflow_id);
    let bound_run = delegation_task_run::Entity::find_by_id(semantic_task_id)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        bound_run.work_unit_key.as_deref(),
        Some(work_unit_key.as_str())
    );

    materialize_v2_fixture_run(
        &db,
        semantic_task_id,
        "Aggregate semantic channel.\n\nConclusion: done",
    )
    .await;
    let semantic_run = delegation_task_run::Entity::find_by_id(semantic_task_id)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    assert!(semantic_run.completion_evidence_json.is_some());
    assert!(semantic_run.card_summary_json.is_none());
    assert_eq!(
        semantic_run.status,
        delegation_task_run::DelegationRunStatus::Completed
    );

    let historical_parent = 201;
    let historical_workflow_id = "wf-task-10-aggregate-historical-v1";
    let historical_db = historical_completion_protocol_db(&[historical_workflow_seed(
        historical_workflow_id,
        historical_parent,
        1,
        CompletionProtocolMode::V1,
        None,
    )])
    .await;
    let _historical_publication = seed_historical_manifest(
        &historical_db,
        historical_workflow_id,
        &skeleton("task-10-aggregate-historical"),
    )
    .await;
    let historical_graph = project_workflow_graph_core(&historical_db, historical_parent)
        .await
        .unwrap();
    let historical = historical_graph
        .completion_protocol
        .expect("historical v1 projection must surface protocol header");
    assert_eq!(historical.version, 1);
    assert_eq!(historical.mode, CompletionProtocolMode::V1);
    assert_eq!(historical.creation_mode, historical.mode);

    let mut legacy_document = skeleton("task-10-aggregate-historical");
    legacy_document.workflow_id = Some(historical_workflow_id.into());
    legacy_document.expected_manifest_revision = Some(1);
    legacy_document.nodes[0].title = Some("must remain read only".into());
    let legacy_mutation = publish_workflow_manifest_core(
        &historical_db,
        &EventEmitter::Noop,
        historical_parent,
        PublishWorkflowRequest {
            document: legacy_document,
        },
    )
    .await;
    assert_eq!(legacy_mutation.unwrap_err().code(), "workflow_v2_retired");

    // Dangling terminal: bind against a live v2 workflow, delete the header,
    // then settle through broker so row/wait/event share one stable code.
    let dangling_workspace = tempfile::tempdir().expect("dangling workspace");
    let dangling_plan = dangling_workspace
        .path()
        .join("docs/superpowers/plans/restarted-plan.md");
    let dangling_design = dangling_workspace.path().join(design_rel);
    std::fs::create_dir_all(dangling_plan.parent().unwrap()).unwrap();
    std::fs::create_dir_all(dangling_design.parent().unwrap()).unwrap();
    std::fs::write(&dangling_plan, b"# Plan\n\nDangling aggregate.\n").unwrap();
    std::fs::write(&dangling_design, design_bytes).unwrap();
    let dangling_db = Arc::new(fresh_in_memory_db().await);
    let dangling_folder =
        seed_folder(&dangling_db, dangling_workspace.path().to_str().unwrap()).await;
    let dangling_parent = seed_conversation(&dangling_db, dangling_folder, AgentType::Codex).await;
    let mut dangling_document = skeleton("task-10-aggregate-dangling");
    dangling_document.design = Some(DocumentRef {
        rel_path: design_rel.into(),
        digest: format!("sha256:{:x}", Sha256::digest(design_bytes)),
    });
    let dangling_published = publish_workflow_manifest_fixture(
        &dangling_db,
        &EventEmitter::Noop,
        dangling_parent,
        PublishWorkflowRequest {
            document: dangling_document,
        },
    )
    .await
    .unwrap();
    let (_runs, events, broker, mock) = aggregate_broker(dangling_db.clone()).await;
    mock.queue_spawn(Ok("aggregate-dangling-child".into()))
        .await;
    mock.queue_send(Ok(accepted(0, Utc::now()))).await;
    let dangling_request = DelegationRequest {
        parent_connection_id: "aggregate-parent".into(),
        parent_conversation_id: dangling_parent,
        parent_tool_use_id: "task-10-aggregate-dangling".into(),
        agent_type: AgentType::Codex,
        profile_id: None,
        task: "dangling terminal aggregate".into(),
        working_dir: Some(dangling_workspace.path().to_string_lossy().into_owned()),
        requested_working_dir: None,
        external_handle: None,
        work_unit_key: Some(work_unit_key.clone()),
        replaces_task_id: None,
        replacement_reason: None,
        correlation_id: None,
        recovery_authorization_id: None,
        orchestration_binding: None,
    };
    let dangling_report =
        with_historical_workflow_fixture_mutations(broker.start_delegation(dangling_request)).await;
    assert_eq!(
        dangling_report.status,
        TaskStatus::Running,
        "{dangling_report:?}"
    );
    let dangling_task_id = dangling_report.task_id.expect("running task id");
    let dangling_binding = delegation_workflow_run_binding::Entity::find_by_id(&dangling_task_id)
        .one(&dangling_db.conn)
        .await
        .unwrap()
        .expect("workflow-bound run before dangling delete");
    assert_eq!(dangling_binding.workflow_id, dangling_published.workflow_id);

    dangling_db
        .conn
        .execute(Statement::from_string(
            DbBackend::Sqlite,
            "PRAGMA foreign_keys = OFF".to_string(),
        ))
        .await
        .unwrap();
    dangling_db
        .conn
        .execute(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "DELETE FROM delegation_workflows WHERE workflow_id = ?",
            vec![dangling_published.workflow_id.clone().into()],
        ))
        .await
        .unwrap();
    dangling_db
        .conn
        .execute(Statement::from_string(
            DbBackend::Sqlite,
            "PRAGMA foreign_keys = ON".to_string(),
        ))
        .await
        .unwrap();

    broker
        .complete_call(
            &dangling_task_id,
            DelegationOutcome::Ok(DelegationSuccess {
                text: "Conclusion: done\n<!-- codeg-card-summary-v1\n{\"kind\":\"author\",\"status\":\"done\",\"plan_digest\":\"sha256:dangling\"}\n-->".into(),
                child_conversation_id: 0,
                child_agent_type: AgentType::Codex,
                turn_count: 1,
                duration_ms: 10,
                token_usage: None,
            }),
        )
        .await;

    let dangling_row = delegation_task_run::Entity::find_by_id(&dangling_task_id)
        .one(&dangling_db.conn)
        .await
        .unwrap()
        .unwrap();
    let wait_report = broker
        .get_task_status(
            "aggregate-parent",
            Some(dangling_parent),
            &dangling_task_id,
            StatusWait::Snapshot,
        )
        .await;
    let emit_calls = events.snapshot().await;
    let completed = emit_calls
        .iter()
        .find(|call| call.task_id == dangling_task_id)
        .expect("dangling terminal event");
    let event_code = match &completed.result {
        DelegationResultSummary::Err { error_code, .. } => error_code.clone(),
        other => panic!("expected typed dangling failure event, got {other:?}"),
    };
    let dangling = DanglingTerminalCodes {
        row_code: dangling_row
            .error_code
            .clone()
            .expect("dangling durable row code"),
        wait_code: wait_report
            .error_code
            .clone()
            .expect("dangling wait report code"),
        event_code,
    };
    assert_eq!(dangling.row_code, "unsupported_completion_protocol");
    assert_eq!(dangling.wait_code, dangling.row_code);
    assert_eq!(dangling.event_code, dangling.row_code);
    assert_eq!(
        dangling_row.status,
        delegation_task_run::DelegationRunStatus::Failed
    );
    assert!(dangling_row.card_summary_json.is_none());

    // Standalone (no workflow binding) still materializes display Card JSON.
    let standalone_workspace = tempfile::tempdir().expect("standalone workspace");
    let standalone_db = Arc::new(fresh_in_memory_db().await);
    let standalone_folder = seed_folder(
        &standalone_db,
        standalone_workspace.path().to_str().unwrap(),
    )
    .await;
    let standalone_parent =
        seed_conversation(&standalone_db, standalone_folder, AgentType::Codex).await;
    let (_runs, _events, standalone_broker, standalone_mock) =
        aggregate_broker(standalone_db.clone()).await;
    standalone_mock
        .queue_spawn(Ok("aggregate-standalone-child".into()))
        .await;
    standalone_mock
        .queue_send(Ok(accepted(0, Utc::now())))
        .await;
    let standalone_report = standalone_broker
        .start_delegation(DelegationRequest {
            parent_connection_id: "aggregate-standalone-parent".into(),
            parent_conversation_id: standalone_parent,
            parent_tool_use_id: "task-10-aggregate-standalone".into(),
            agent_type: AgentType::Codex,
            profile_id: None,
            task: "standalone card aggregate".into(),
            working_dir: Some(standalone_workspace.path().to_string_lossy().into_owned()),
            requested_working_dir: None,
            external_handle: None,
            work_unit_key: None,
            replaces_task_id: None,
            replacement_reason: None,
            correlation_id: None,
            recovery_authorization_id: None,
            orchestration_binding: None,
        })
        .await;
    assert_eq!(
        standalone_report.status,
        TaskStatus::Running,
        "{standalone_report:?}"
    );
    let standalone_task_id = standalone_report.task_id.expect("standalone task id");
    assert!(
        delegation_workflow_run_binding::Entity::find_by_id(&standalone_task_id)
            .one(&standalone_db.conn)
            .await
            .unwrap()
            .is_none(),
        "standalone run must not create a workflow binding"
    );
    standalone_broker
        .complete_call(
            &standalone_task_id,
            DelegationOutcome::Ok(DelegationSuccess {
                text: r#"review body
<!-- codeg-card-summary-v1
{"kind":"review","verdict":"approve","critical":0,"important":0,"minor":0,"summary":"Standalone card retained."}
-->"#
                    .into(),
                child_conversation_id: 0,
                child_agent_type: AgentType::Codex,
                turn_count: 1,
                duration_ms: 12,
                token_usage: None,
            }),
        )
        .await;
    let standalone = delegation_task_run::Entity::find_by_id(&standalone_task_id)
        .one(&standalone_db.conn)
        .await
        .unwrap()
        .unwrap();
    assert!(standalone.card_summary_json.is_some());
    assert_eq!(
        standalone.status,
        delegation_task_run::DelegationRunStatus::Completed
    );
}
