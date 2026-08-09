use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum_test::TestServer;
use codeg_lib::acp::delegation::broker::{
    compare_completion_shadow_outcome, is_completion_format_repair_prompt,
    pre_read_completion_reports_for_test, ConversationDepthLookup, DelegationBroker,
};
use codeg_lib::acp::delegation::companion::{
    dispatch_line, CompanionContext, CompanionFeatures, InflightCalls, LineAction, TOOL_SCHEMA_JSON,
};
use codeg_lib::acp::delegation::lease::CompanionLeaseRegistry;
use codeg_lib::acp::delegation::listener::{
    DelegationListener, ParentSessionLookup, TokenEntry, TokenRegistry,
};
use codeg_lib::acp::delegation::metrics::{
    CompletionContinuationReason, CompletionFinalMetricState, CompletionRestartOutcome,
    CompletionShadowDifference, DelegationMetrics,
};
use codeg_lib::acp::delegation::run_store::{ReservingRunInsert, RunStore};
use codeg_lib::acp::delegation::spawner::{mock::MockSpawner, ConnectionSpawner, SpawnerError};
use codeg_lib::acp::delegation::transport::{
    client_cancel_task_round_trip, client_get_workflow_state_round_trip, client_status_round_trip,
    BrokerCancelTaskRequest, BrokerGetWorkflowStateRequest, BrokerStatusRequest,
    CancelDelegationReason, CompanionRole,
};
use codeg_lib::acp::delegation::types::{
    CompletionMutationContext, ContinueDelegationRequest, DelegationError,
    ResolveCompletionDecisionRequest, RestartLegacyWorkflowRequest,
};
use codeg_lib::acp::delegation::workflow::{
    build_work_unit_key, capture_original_request_context, evaluate_rollout_window,
    get_workflow_state_core, guard_current_final_delivery_core, guard_final_delivery_core,
    guard_task_final_delivery_core, inject_legacy_restart_header_failure_once,
    load_completion_protocol_for_conversation, materialize_terminal_completion_txn,
    project_workflow_graph_core, publish_workflow_manifest_core, recover_workflow_core,
    restart_legacy_workflow_core, restart_legacy_workflow_if_enforced, select_completion_protocol,
    settle_workflow_gate_v2_core, CompletionCardV2, CompletionIntent, CompletionIntentSource,
    CompletionOutcome, CompletionProtocolConfigurationRemoved, CompletionProtocolRolloutConfig,
    CompletionProtocolSelection, CompletionResolution, CompletionRole, DocumentGateKind,
    DocumentRef, FinalDeliveryGuardRequest, FinalDeliveryGuardResult, ManifestDocument,
    ManifestGate, ManifestNode, ManifestNodeKind, ManifestNodeRole, ManifestPhase,
    ManifestWorkflowState, PlanReviewChangeV2, PlanReviewNextAction, ProfileCompletionWindow,
    PublishWorkflowRequest, RecoverWorkflowRequest, ResolutionMode, RolloutDecision,
    SettleWorkflowV2Request, TerminalCompletionInput, WorkUnitKeyParts, WorkflowStoreError,
    CURRENT_COMPLETION_PROTOCOL_VERSION, MANIFEST_SCHEMA_VERSION, PHASE_DESIGN, PHASE_FINAL,
    PHASE_PLAN, WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY,
};
use codeg_lib::acp::error::AcpError;
use codeg_lib::acp::manager::ConnectionManager;
use codeg_lib::acp::question::{QuestionSpec, RegisteredQuestion, SessionQuestionAccess};
use codeg_lib::acp::types::PromptInputBlock;
use codeg_lib::app_state::AppState;
use codeg_lib::commands::workflow_completion::restart_legacy_workflow_authenticated_core;
use codeg_lib::db::entities::delegation_workflow_gate_settlement::GateSettlementOutcome;
use codeg_lib::db::entities::{
    auto_title_job, delegation_attention_request, delegation_completion_tool_intent,
    delegation_task_run, delegation_workflow, delegation_workflow_gate_settlement,
    delegation_workflow_gate_state, delegation_workflow_manifest_revision,
    delegation_workflow_node_binding, delegation_workflow_restart_context,
    delegation_workflow_run_binding, recovery_authorization,
};
use codeg_lib::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
use codeg_lib::models::AgentType;
use codeg_lib::web::auth::COMPLETION_CONTEXT_HEADER;
use codeg_lib::web::event_bridge::EventEmitter;
use codeg_lib::web::router::build_router;
use codeg_lib::web::shutdown::ShutdownSignal;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, PaginatorTrait,
    QueryFilter, Set, Statement, TransactionTrait,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

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

        let published = publish_workflow_manifest_core(
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
        let revised = publish_workflow_manifest_core(
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
    let db = fresh_in_memory_db().await;
    let folder = seed_folder(&db, "/tmp/task-2-fixed-v2-historical-revision").await;
    let parent = seed_conversation(&db, folder, AgentType::Codex).await;
    let mut document = skeleton("task-2-fixed-v2-historical-revision");
    let published = publish_workflow_manifest_core(
        &db,
        &EventEmitter::Noop,
        parent,
        PublishWorkflowRequest {
            document: document.clone(),
        },
    )
    .await
    .unwrap();
    mark_historical_completion_protocol(
        &db,
        &published.workflow_id,
        delegation_workflow::CompletionProtocolMode::V1,
    )
    .await;

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
    assert_eq!(error.code(), "legacy_completion_protocol_read_only");

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
    std::env::temp_dir().join(format!("codeg-task-18-final-{}.sock", uuid::Uuid::new_v4()))
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

async fn mark_historical_completion_protocol(
    db: &codeg_lib::db::AppDatabase,
    workflow_id: &str,
    mode: delegation_workflow::CompletionProtocolMode,
) {
    let row = delegation_workflow::Entity::find_by_id(workflow_id)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    let mut row: delegation_workflow::ActiveModel = row.into();
    row.completion_protocol_version = Set(1);
    row.completion_protocol_mode = Set(mode);
    row.update(&db.conn).await.unwrap();
}

async fn set_completion_protocol_pair(
    db: &codeg_lib::db::AppDatabase,
    workflow_id: &str,
    version: i64,
    mode: delegation_workflow::CompletionProtocolMode,
) {
    let row = delegation_workflow::Entity::find_by_id(workflow_id)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    let mut row: delegation_workflow::ActiveModel = row.into();
    row.completion_protocol_version = Set(version);
    row.completion_protocol_mode = Set(mode);
    row.update(&db.conn).await.unwrap();
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
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, workspace.path().to_str().unwrap()).await;
        let parent = seed_conversation(&db, folder, AgentType::Codex).await;
        let child = seed_conversation(&db, folder, AgentType::Codex).await;
        let mut document = skeleton(&format!("task-4-mutation-matrix-{index}"));
        let published = publish_workflow_manifest_core(
            &db,
            &EventEmitter::Noop,
            parent,
            PublishWorkflowRequest {
                document: document.clone(),
            },
        )
        .await
        .unwrap();
        let task_id = format!("task-4-final-guard-{index}");
        seed_final_guard_binding(&db, parent, child, &published.workflow_id, &task_id).await;
        set_completion_protocol_pair(&db, &published.workflow_id, version, mode).await;
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
        assert_eq!(publish_error.code(), expected_code);

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
async fn historical_protocol_cross_parent_mutations_remain_unauthorized() {
    let db = fresh_in_memory_db().await;
    let owner_folder = seed_folder(&db, "/tmp/task-4-owner-fence").await;
    let foreign_folder = seed_folder(&db, "/tmp/task-4-foreign-fence").await;
    let owner = seed_conversation(&db, owner_folder, AgentType::Codex).await;
    let foreign = seed_conversation(&db, foreign_folder, AgentType::Codex).await;
    let published = publish_workflow_manifest_core(
        &db,
        &EventEmitter::Noop,
        owner,
        PublishWorkflowRequest {
            document: skeleton("task-4-cross-parent-protocol-fence"),
        },
    )
    .await
    .unwrap();
    set_completion_protocol_pair(
        &db,
        &published.workflow_id,
        1,
        delegation_workflow::CompletionProtocolMode::V1,
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
    assert_eq!(publication_error.code(), "unauthorized");

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

#[tokio::test]
async fn corrupt_header_nonterminal_fences() {
    for (index, version, mode) in [(0, 99, "v2_enforce"), (1, 2, "corrupt_mode")] {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, &format!("/tmp/task-4-corrupt-header-{index}")).await;
        let parent = seed_conversation(&db, folder, AgentType::Codex).await;
        let mut document = skeleton(&format!("task-4-corrupt-header-{index}"));
        let published = publish_workflow_manifest_core(
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
        assert_eq!(publish_error.code(), "unsupported_completion_protocol");

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
        manager.install_completion_protocol_runtime(
            Arc::new(CompletionProtocolRolloutConfig {
                default_mode: delegation_workflow::CompletionProtocolMode::V2Enforce,
                ..Default::default()
            }),
            Arc::new(DelegationMetrics::default()),
        );
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
    let published = publish_workflow_manifest_core(
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

    mark_historical_completion_protocol(
        &db,
        &published.workflow_id,
        delegation_workflow::CompletionProtocolMode::V1,
    )
    .await;
    let settlements_before = delegation_workflow_gate_settlement::Entity::find()
        .filter(delegation_workflow_gate_settlement::Column::WorkflowId.eq(&published.workflow_id))
        .count(&db.conn)
        .await
        .unwrap();
    let legacy_error = settle_workflow_gate_v2_core(
        &db,
        &EventEmitter::Noop,
        parent,
        SettleWorkflowV2Request {
            workflow_id: published.workflow_id.clone(),
            gate_id: "design".into(),
            expected_graph_revision: published.graph_revision,
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
                delegation_workflow_gate_settlement::Column::WorkflowId.eq(&published.workflow_id),
            )
            .count(&db.conn)
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
    let enforced = publish_workflow_manifest_core(&db, &EventEmitter::Noop, parent, request)
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
    let title_result = publish_workflow_manifest_core(
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
    publish_workflow_manifest_core(
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
    let published = publish_workflow_manifest_core(
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
    let added = publish_workflow_manifest_core(
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
    publish_workflow_manifest_core(
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
    let published = publish_workflow_manifest_core(
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
    let added = publish_workflow_manifest_core(
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
    publish_workflow_manifest_core(
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
    runs.admit_gen1_reserving(ReservingRunInsert {
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
    })
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
    capture_original_request_context(
        &db.conn,
        parent,
        "task-18-capability-request",
        &[PromptInputBlock::Text {
            text: "Prove the platform completion capability matrix.".into(),
        }],
        "codex",
    )
    .await
    .unwrap();
    let token = format!("task-18-capability-{}", uuid::Uuid::new_v4());
    let mut document = skeleton(&token);
    document.design = Some(DocumentRef {
        rel_path: DESIGN_REL_PATH.into(),
        digest: format!("sha256:{:x}", Sha256::digest(DESIGN_BYTES)),
    });
    let published = publish_workflow_manifest_core(
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
    runs.admit_gen1_reserving(ReservingRunInsert {
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
    })
    .await
    .unwrap();

    let child_connection_id = format!("task-18-capability-child-{task_id}");
    runs.bind_child_connection_while_reserving(&task_id, &child_connection_id)
        .await
        .unwrap();
    runs.promote_running(&task_id, &child_connection_id, chrono::Utc::now())
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
        parent_connection_id: child_connection_id,
        socket_path: socket_path.to_string_lossy().into_owned(),
        token: child_token,
        features: CompanionFeatures::parse(Some("completion_v2")),
        role: CompanionRole::DelegationChild,
        connection_incarnation_id: format!("incarnation-{task_id}"),
        disabled_agents: Vec::new(),
    };
    let root_companion = CompanionContext {
        parent_connection_id,
        socket_path: socket_path.to_string_lossy().into_owned(),
        token: root_token,
        features: CompanionFeatures::parse(Some("delegation,workflow_v2")),
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
        let arguments = json!({
            "outcome": "done",
            "summary": "tool completion",
        });
        let stable_tool_call_id = format!("complete-work-{task_id}");
        let first = call_companion_tool(
            &child_companion,
            7,
            "complete_work",
            arguments.clone(),
            Some(&stable_tool_call_id),
        )
        .await;
        let replay = call_companion_tool(
            &child_companion,
            8,
            "complete_work",
            arguments,
            Some(&stable_tool_call_id),
        )
        .await;
        assert_eq!(
            first["structuredContent"]["intent_id"],
            replay["structuredContent"]["intent_id"]
        );
        assert_eq!(first["structuredContent"]["accepted_ordinal"], 1);
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
        let context_response = server
            .post("/api/get_workflow_graph_snapshot")
            .add_header("authorization", "Bearer task-18-token")
            .json(&json!({ "conversationId": parent }))
            .await;
        context_response.assert_status_ok();
        let context = context_response
            .headers()
            .get(COMPLETION_CONTEXT_HEADER)
            .expect("snapshot must issue a completion context")
            .to_str()
            .unwrap()
            .to_string();
        let response = server
            .post("/api/resolve_completion_decision")
            .add_header("authorization", "Bearer task-18-token")
            .add_header(COMPLETION_CONTEXT_HEADER, context)
            .json(&ResolveCompletionDecisionRequest {
                cas: terminal.attention.expect("ambiguous completion decision"),
                outcome: CompletionOutcome::Done,
            })
            .await;
        response.assert_status_ok();
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
async fn every_model_capability_reaches_one_platform_completion_truth() {
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
    let published = publish_workflow_manifest_core(
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
    let continuation = broker
        .continue_delegation(ContinueDelegationRequest {
            parent_connection_id: "session-2889-parent".into(),
            parent_conversation_id: parent,
            parent_tool_use_id: "session-2889-card-reemit".into(),
            target_task_id: first_reviewer_task_id.into(),
            task: "CARD RE-EMIT ONLY".into(),
            work_unit_key: first_reviewer.work_unit_key.clone(),
            external_handle: None,
            correlation_id: None,
            recovery_authorization_id: None,
        })
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
    let published = publish_workflow_manifest_core(
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
    let listener_task = tokio::spawn(listener.run(socket_path.clone()));
    let response = match surface {
        FinalEnrichmentSurface::Report => {
            client_cancel_task_round_trip(
                socket_path.to_string_lossy().as_ref(),
                &BrokerCancelTaskRequest {
                    token: root_token.into(),
                    task_id: task_id.into(),
                    reason: CancelDelegationReason::TaskFail,
                },
            )
            .await
            .unwrap()
            .outcome
        }
        FinalEnrichmentSurface::Status => {
            client_status_round_trip(
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
            .unwrap()
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

async fn legacy_source() -> (codeg_lib::db::AppDatabase, i32, String) {
    let db = fresh_in_memory_db().await;
    let folder = seed_folder(&db, "/tmp/task-15-legacy-restart").await;
    let parent = seed_conversation(&db, folder, AgentType::Codex).await;
    capture_original_request_context(
        &db.conn,
        parent,
        "original-turn-1",
        &[PromptInputBlock::Text {
            text: "implement the original Task 15 request".into(),
        }],
        "codex",
    )
    .await
    .unwrap();
    let published = publish_workflow_manifest_core(
        &db,
        &EventEmitter::Noop,
        parent,
        PublishWorkflowRequest {
            document: skeleton("task-15-legacy-source"),
        },
    )
    .await
    .unwrap();
    mark_historical_completion_protocol(
        &db,
        &published.workflow_id,
        delegation_workflow::CompletionProtocolMode::V1,
    )
    .await;
    (db, parent, published.workflow_id)
}

#[tokio::test]
async fn legacy_restart_preserves_non_default_context_but_routes_codex_plan_author() {
    let db = fresh_in_memory_db().await;
    let folder = seed_folder(&db, "/tmp/task-15-request-context").await;
    let parent = seed_conversation(&db, folder, AgentType::Grok).await;
    assert!(
        codeg_lib::db::entities::auto_title_job::Entity::find_by_id(parent)
            .one(&db.conn)
            .await
            .unwrap()
            .is_none()
    );
    let original_request = "diagnose the rollout and preserve this exact job request";
    capture_original_request_context(
        &db.conn,
        parent,
        "grok-user-turn-77",
        &[PromptInputBlock::Text {
            text: original_request.into(),
        }],
        "grok",
    )
    .await
    .unwrap();

    let published = publish_workflow_manifest_core(
        &db,
        &EventEmitter::Noop,
        parent,
        PublishWorkflowRequest {
            document: skeleton("task-15-grok-profile-source"),
        },
    )
    .await
    .unwrap();
    mark_historical_completion_protocol(
        &db,
        &published.workflow_id,
        delegation_workflow::CompletionProtocolMode::V1,
    )
    .await;
    let source_author = delegation_workflow_node_binding::Entity::find()
        .filter(delegation_workflow_node_binding::Column::WorkflowId.eq(&published.workflow_id))
        .filter(delegation_workflow_node_binding::Column::Role.eq("author"))
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    let mut source_author: delegation_workflow_node_binding::ActiveModel = source_author.into();
    source_author.agent_type = Set("grok".into());
    source_author.profile_id = Set(Some("review-canary".into()));
    source_author.work_unit_key = Set(build_work_unit_key(&WorkUnitKeyParts::PlanAuthor {
        rel_plan_path: "docs/superpowers/plans/restarted-plan.md",
        agent_type: "grok",
        profile_id: Some("review-canary"),
    })
    .unwrap());
    source_author.update(&db.conn).await.unwrap();

    let restarted = restart_legacy_workflow_core(&db, i64::from(parent))
        .await
        .unwrap();
    assert_eq!(
        restarted.restart_context.original_request_id,
        "grok-user-turn-77"
    );
    assert_eq!(
        restarted.restart_context.original_request_text,
        original_request
    );
    assert!(restarted
        .restart_context
        .original_request_digest
        .starts_with("sha256:"));
    assert_eq!(restarted.restart_context.agent_type, "grok");
    assert_eq!(
        restarted.restart_context.profile_id.as_deref(),
        Some("review-canary")
    );
    let successor_author = delegation_workflow_node_binding::Entity::find()
        .filter(
            delegation_workflow_node_binding::Column::WorkflowId
                .eq(&restarted.successor_workflow_id),
        )
        .filter(delegation_workflow_node_binding::Column::Role.eq("author"))
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(successor_author.agent_type, "codex");
    assert_eq!(successor_author.profile_id, None);
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
async fn legacy_restart_enforce_resume_creates_one_empty_v2_successor_and_never_mutates_source() {
    let (db, parent, source_workflow_id) = legacy_source().await;
    let before = source_fingerprint(&db, parent, &source_workflow_id).await;

    let first = restart_legacy_workflow_core(&db, i64::from(parent))
        .await
        .unwrap();
    let replay = restart_legacy_workflow_core(&db, i64::from(parent))
        .await
        .unwrap();

    assert_eq!(
        first.successor_conversation_id,
        replay.successor_conversation_id
    );
    assert_eq!(
        source_fingerprint(&db, parent, &source_workflow_id).await,
        before
    );
    let successors = delegation_workflow::Entity::find()
        .filter(delegation_workflow::Column::LegacySourceWorkflowId.eq(&source_workflow_id))
        .all(&db.conn)
        .await
        .unwrap();
    assert_eq!(successors.len(), 1);
    assert_eq!(successors[0].completion_protocol_version, 2);
    assert_eq!(
        successors[0].completion_protocol_mode,
        delegation_workflow::CompletionProtocolMode::V2Enforce
    );
    assert_eq!(first.open_gate.as_str(), "design");
    let source_graph = project_workflow_graph_core(&db, parent).await.unwrap();
    let source_protocol = source_graph.completion_protocol.unwrap();
    assert_eq!(
        source_protocol
            .v2_successor
            .as_ref()
            .map(|link| link.conversation_id),
        Some(first.successor_conversation_id)
    );
    assert_eq!(
        source_protocol.read_only_reason.as_deref(),
        Some("legacy_completion_protocol_restart_required")
    );
    assert!(!source_protocol.automatic_root_wake);
    let source_state = get_workflow_state_core(&db, parent, Some(&source_workflow_id))
        .await
        .unwrap();
    assert_eq!(
        source_state
            .completion_protocol
            .v2_successor
            .as_ref()
            .map(|link| link.conversation_id),
        Some(first.successor_conversation_id)
    );
    let successor_graph = project_workflow_graph_core(&db, first.successor_conversation_id)
        .await
        .unwrap();
    assert_eq!(
        successor_graph
            .completion_protocol
            .unwrap()
            .legacy_source
            .as_ref()
            .map(|link| link.conversation_id),
        Some(parent)
    );
    assert_eq!(
        delegation_task_run::Entity::find()
            .filter(
                delegation_task_run::Column::ParentConversationId
                    .eq(first.successor_conversation_id)
            )
            .count(&db.conn)
            .await
            .unwrap(),
        0
    );
    for count in [
        delegation_workflow_run_binding::Entity::find()
            .filter(
                delegation_workflow_run_binding::Column::WorkflowId.eq(&successors[0].workflow_id),
            )
            .count(&db.conn)
            .await
            .unwrap(),
        delegation_workflow_gate_settlement::Entity::find()
            .filter(
                delegation_workflow_gate_settlement::Column::WorkflowId
                    .eq(&successors[0].workflow_id),
            )
            .count(&db.conn)
            .await
            .unwrap(),
        delegation_attention_request::Entity::find()
            .filter(
                delegation_attention_request::Column::ParentConversationId
                    .eq(first.successor_conversation_id),
            )
            .count(&db.conn)
            .await
            .unwrap(),
    ] {
        assert_eq!(count, 0);
    }
}

#[tokio::test]
async fn legacy_restart_failed_creation_leaves_source_unchanged_and_is_retryable() {
    let (db, parent, source_workflow_id) = legacy_source().await;
    let before = source_fingerprint(&db, parent, &source_workflow_id).await;
    inject_legacy_restart_header_failure_once();

    let error = restart_legacy_workflow_core(&db, i64::from(parent))
        .await
        .unwrap_err();
    assert_eq!(error.code(), "legacy_completion_protocol_restart_required");
    assert!(error.is_retryable());
    assert_eq!(
        source_fingerprint(&db, parent, &source_workflow_id).await,
        before
    );
    assert_eq!(
        delegation_workflow::Entity::find()
            .filter(delegation_workflow::Column::LegacySourceWorkflowId.eq(&source_workflow_id))
            .count(&db.conn)
            .await
            .unwrap(),
        0
    );
    assert!(restart_legacy_workflow_core(&db, i64::from(parent))
        .await
        .is_ok());
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
        let db = fresh_in_memory_db().await;
        let folder_id = seed_folder(&db, &format!("/tmp/task-4-root-fence-{index}")).await;
        let parent = seed_conversation(&db, folder_id, AgentType::Codex).await;
        capture_original_request_context(
            &db.conn,
            parent,
            &format!("task-4-root-original-{index}"),
            &[PromptInputBlock::Text {
                text: "original request must remain immutable".into(),
            }],
            "codex",
        )
        .await
        .unwrap();
        let published = publish_workflow_manifest_core(
            &db,
            &EventEmitter::Noop,
            parent,
            PublishWorkflowRequest {
                document: skeleton(&format!("task-4-root-fence-{index}")),
            },
        )
        .await
        .unwrap();
        set_completion_protocol_pair(&db, &published.workflow_id, version, mode).await;
        let before = mutation_snapshot(&db, parent, &published.workflow_id).await;

        let manager = ConnectionManager::new();
        manager.install_completion_protocol_runtime(
            Arc::new(CompletionProtocolRolloutConfig {
                default_mode: delegation_workflow::CompletionProtocolMode::V2Enforce,
                ..Default::default()
            }),
            Arc::new(DelegationMetrics::default()),
        );
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
    let db = fresh_in_memory_db().await;
    let folder = seed_folder(&db, "/tmp/task-4-root-multi-generation").await;
    let workflow_parent = seed_conversation(&db, folder, AgentType::Codex).await;
    let child = seed_conversation(&db, folder, AgentType::Codex).await;
    let published = publish_workflow_manifest_core(
        &db,
        &EventEmitter::Noop,
        workflow_parent,
        PublishWorkflowRequest {
            document: skeleton("task-4-root-multi-generation"),
        },
    )
    .await
    .unwrap();
    mark_historical_completion_protocol(
        &db,
        &published.workflow_id,
        delegation_workflow::CompletionProtocolMode::V1,
    )
    .await;
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
async fn root_protocol_loader_rejects_bound_legacy_when_conversation_owns_v2() {
    let db = fresh_in_memory_db().await;
    let folder = seed_folder(&db, "/tmp/task-4-root-owned-bound-conflict").await;
    let legacy_parent = seed_conversation(&db, folder, AgentType::Codex).await;
    let child = seed_conversation(&db, folder, AgentType::Codex).await;
    let legacy = publish_workflow_manifest_core(
        &db,
        &EventEmitter::Noop,
        legacy_parent,
        PublishWorkflowRequest {
            document: skeleton("task-4-root-bound-legacy"),
        },
    )
    .await
    .unwrap();
    mark_historical_completion_protocol(
        &db,
        &legacy.workflow_id,
        delegation_workflow::CompletionProtocolMode::V1,
    )
    .await;
    let owned = publish_workflow_manifest_core(
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
        .unwrap();
    assert_eq!(
        first,
        Some((1, delegation_workflow::CompletionProtocolMode::V1,)),
        "owned v2 workflow must not mask a rejecting bound legacy workflow"
    );
    assert_eq!(
        load_completion_protocol_for_conversation(&db, child)
            .await
            .unwrap(),
        first,
        "conflicting durable associations must resolve deterministically"
    );
    assert_ne!(legacy.workflow_id, owned.workflow_id);
}

#[tokio::test]
async fn legacy_restart_upgrade_is_rejected_before_resume_side_effects() {
    let db = fresh_in_memory_db().await;
    let folder = seed_folder(&db, "/tmp/task-15-legacy-upgrade").await;
    let parent = seed_conversation(&db, folder, AgentType::Codex).await;
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
    let published = publish_workflow_manifest_core(
        &db,
        &EventEmitter::Noop,
        parent,
        PublishWorkflowRequest {
            document: skeleton("task-15-pre-migration-source"),
        },
    )
    .await
    .unwrap();
    mark_historical_completion_protocol(
        &db,
        &published.workflow_id,
        delegation_workflow::CompletionProtocolMode::V1,
    )
    .await;
    assert!(
        delegation_workflow_restart_context::Entity::find_by_id(parent)
            .one(&db.conn)
            .await
            .unwrap()
            .is_none()
    );
    let before = source_fingerprint(&db, parent, &published.workflow_id).await;

    let metrics = std::sync::Arc::new(DelegationMetrics::default());
    let rollout = CompletionProtocolRolloutConfig {
        default_mode: delegation_workflow::CompletionProtocolMode::V2Enforce,
        ..Default::default()
    };
    let manager = ConnectionManager::new();
    manager.install_completion_protocol_runtime(std::sync::Arc::new(rollout), metrics);
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
async fn legacy_restart_upgrade_without_durable_request_remains_fail_closed() {
    let db = fresh_in_memory_db().await;
    let folder = seed_folder(&db, "/tmp/task-15-legacy-upgrade-no-context").await;
    let parent = seed_conversation(&db, folder, AgentType::Codex).await;
    let published = publish_workflow_manifest_core(
        &db,
        &EventEmitter::Noop,
        parent,
        PublishWorkflowRequest {
            document: skeleton("task-15-pre-migration-no-context"),
        },
    )
    .await
    .unwrap();
    mark_historical_completion_protocol(
        &db,
        &published.workflow_id,
        delegation_workflow::CompletionProtocolMode::V1,
    )
    .await;
    let before = source_fingerprint(&db, parent, &published.workflow_id).await;
    let enforce = CompletionProtocolRolloutConfig {
        default_mode: delegation_workflow::CompletionProtocolMode::V2Enforce,
        ..Default::default()
    };

    let error = restart_legacy_workflow_if_enforced(&db, parent, None, &enforce)
        .await
        .expect_err("missing historical request bytes must remain fail-closed");

    assert_eq!(error.code(), "legacy_completion_protocol_restart_required");
    assert!(error.to_string().contains("context is unavailable"));
    assert_eq!(
        source_fingerprint(&db, parent, &published.workflow_id).await,
        before
    );
    assert_eq!(
        delegation_workflow::Entity::find()
            .filter(delegation_workflow::Column::LegacySourceWorkflowId.eq(&published.workflow_id),)
            .count(&db.conn)
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn fixed_v2_creation_is_not_affected_by_rollout_configuration() {
    let db = fresh_in_memory_db().await;
    let folder = seed_folder(&db, "/tmp/task-15-frozen-rollout").await;
    let parent = seed_conversation(&db, folder, AgentType::Codex).await;
    let mut config = CompletionProtocolRolloutConfig {
        default_mode: delegation_workflow::CompletionProtocolMode::V2Shadow,
        ..Default::default()
    };
    let selection = select_completion_protocol("codex", Some("canary"), &config);
    assert_eq!(
        selection.mode,
        delegation_workflow::CompletionProtocolMode::V2Shadow
    );
    let published = publish_workflow_manifest_core(
        &db,
        &EventEmitter::Noop,
        parent,
        PublishWorkflowRequest {
            document: skeleton("task-15-shadow"),
        },
    )
    .await
    .unwrap();

    config.default_mode = delegation_workflow::CompletionProtocolMode::V1;
    let row = delegation_workflow::Entity::find_by_id(published.workflow_id)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.completion_protocol_version, 2);
    assert_eq!(
        row.completion_protocol_mode,
        delegation_workflow::CompletionProtocolMode::V2Enforce
    );
    assert_eq!(
        select_completion_protocol("codex", Some("canary"), &config),
        CompletionProtocolSelection::v1_default()
    );
}

#[tokio::test]
async fn rollout_restart_accepts_stored_shadow_only_when_current_policy_is_enforce() {
    let db = fresh_in_memory_db().await;
    let folder = seed_folder(&db, "/tmp/task-15-shadow-restart").await;
    let parent = seed_conversation(&db, folder, AgentType::Grok).await;
    capture_original_request_context(
        &db.conn,
        parent,
        "stored-shadow-original-request",
        &[PromptInputBlock::Text {
            text: "restart the original shadow workflow under enforce".into(),
        }],
        "grok",
    )
    .await
    .unwrap();
    let published = publish_workflow_manifest_core(
        &db,
        &EventEmitter::Noop,
        parent,
        PublishWorkflowRequest {
            document: skeleton("task-15-stored-shadow-source"),
        },
    )
    .await
    .unwrap();
    mark_historical_completion_protocol(
        &db,
        &published.workflow_id,
        delegation_workflow::CompletionProtocolMode::V2Shadow,
    )
    .await;

    let enforce = CompletionProtocolRolloutConfig {
        default_mode: delegation_workflow::CompletionProtocolMode::V2Enforce,
        ..Default::default()
    };
    assert!(restart_legacy_workflow_if_enforced(
        &db,
        parent,
        Some(("grok".into(), Some("review-canary".into()))),
        &enforce,
    )
    .await
    .unwrap()
    .is_some());

    let (db, parent, source_workflow_id) = legacy_source().await;
    let metrics = DelegationMetrics::default();
    let current_v1 = CompletionProtocolRolloutConfig::default();
    let error = restart_legacy_workflow_authenticated_core(
        &db,
        &metrics,
        &current_v1,
        &CompletionMutationContext::authenticated_for_test(parent, "rollout-test"),
        RestartLegacyWorkflowRequest {
            source_conversation_id: i64::from(parent),
        },
    )
    .await
    .expect_err("explicit restart must not bypass current v1 rollout");
    assert_eq!(
        error.detail.as_deref(),
        Some("legacy_completion_protocol_restart_not_required")
    );
    assert_eq!(
        delegation_workflow::Entity::find()
            .filter(delegation_workflow::Column::LegacySourceWorkflowId.eq(source_workflow_id))
            .count(&db.conn)
            .await
            .unwrap(),
        0
    );
}

#[test]
fn rollout_stops_only_after_minimum_sample_and_strict_thresholds() {
    let decision = |samples, role_mismatch, needs_decision| {
        evaluate_rollout_window(&ProfileCompletionWindow {
            samples,
            role_mismatch,
            needs_decision,
        })
    };
    assert_eq!(decision(99, 50, 50), RolloutDecision::InsufficientSamples);
    assert_eq!(decision(100, 1, 5), RolloutDecision::MayExpand);
    assert_eq!(decision(100, 2, 5), RolloutDecision::StopRoleMismatch);
    assert_eq!(decision(100, 1, 6), RolloutDecision::StopNeedsDecision);
}

#[test]
fn rollout_config_rejects_unknown_modes_and_malformed_override_keys() {
    assert!(CompletionProtocolRolloutConfig::from_serialized_values(
        Some("v2_enforce"),
        Some(r#"{"codex|canary":"v2_shadow"}"#),
    )
    .is_ok());
    assert!(
        CompletionProtocolRolloutConfig::from_serialized_values(Some("best_effort"), None).is_err()
    );
    assert!(CompletionProtocolRolloutConfig::from_serialized_values(
        Some("v1"),
        Some(r#"{"missing-profile-separator":"v2_enforce"}"#),
    )
    .is_err());
}

#[test]
fn restart_tool_schema_is_registered_for_root_only() {
    let schema: Value = serde_json::from_str(TOOL_SCHEMA_JSON).unwrap();
    let restart = schema
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "restart_legacy_workflow")
        .unwrap();
    assert_eq!(restart["inputSchema"]["additionalProperties"], false);
    assert_eq!(
        restart["inputSchema"]["required"],
        serde_json::json!(["source_conversation_id"])
    );
    let context = |role| CompanionContext {
        parent_connection_id: "parent".into(),
        socket_path: "socket".into(),
        token: "token".into(),
        features: CompanionFeatures::parse(Some("workflow_v2")),
        role,
        connection_incarnation_id: "incarnation".into(),
        disabled_agents: Vec::new(),
    };
    assert!(context(CompanionRole::Root).allows_tool("restart_legacy_workflow"));
    assert!(!context(CompanionRole::DelegationChild).allows_tool("restart_legacy_workflow"));
}

#[test]
fn completion_protocol_metrics_are_bounded_and_v2_format_repair_stays_zero() {
    let metrics = DelegationMetrics::default();
    metrics
        .record_completion_protocol_creation(delegation_workflow::CompletionProtocolMode::V2Shadow);
    metrics.record_completion_restart(CompletionRestartOutcome::Created);
    metrics.record_completion_shadow_difference(CompletionShadowDifference::NeedsDecision);
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
    assert_eq!(snapshot.creation_modes["v2_shadow"], 1);
    assert_eq!(snapshot.restart_outcomes["created"], 1);
    assert_eq!(snapshot.shadow_differences["needs_decision"], 1);
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
fn completion_protocol_metrics_compare_authorities_and_bound_profile_rollout_window() {
    let resolved = CompletionResolution::Resolved(CompletionIntent {
        outcome: CompletionOutcome::Approve,
        summary: None,
        report_file: None,
        source: CompletionIntentSource::AssistantConclusion,
    });
    assert_eq!(
        compare_completion_shadow_outcome(Some(CompletionOutcome::Approve), &resolved),
        CompletionShadowDifference::Match
    );
    assert_eq!(
        compare_completion_shadow_outcome(Some(CompletionOutcome::RequestChanges), &resolved),
        CompletionShadowDifference::Outcome
    );

    let metrics = DelegationMetrics::default();
    for _ in 0..98 {
        metrics.record_completion_shadow_sample(
            "grok",
            Some("canary"),
            CompletionShadowDifference::Match,
        );
    }
    for _ in 0..2 {
        metrics.record_completion_shadow_sample(
            "grok",
            Some("canary"),
            CompletionShadowDifference::RoleMismatch,
        );
    }
    let snapshot = metrics.snapshot().completion_protocol;
    let window = &snapshot.rollout_windows["grok|canary"];
    assert_eq!((window.samples, window.role_mismatch), (100, 2));
    assert_eq!(
        snapshot.rollout_decisions["grok|canary"],
        RolloutDecision::StopRoleMismatch
    );
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
