use std::sync::Arc;

use axum_test::TestServer;
use chrono::Utc;
use codeg_lib::acp::delegation::companion::TOOL_SCHEMA_JSON;
use codeg_lib::acp::delegation::run_store::{ReservingRunInsert, RunStore};
use codeg_lib::acp::delegation::transport::BrokerSettleWorkflowRequest;
use codeg_lib::acp::delegation::types::{
    CompletionMutationResult, DelegationReplyResult, ResolveCompletionDecisionRequest,
    ResolveDesignSelfReviewRequest, RetryCompletionArtifactRequest,
};
use codeg_lib::acp::delegation::workflow::store::{
    publish_workflow_manifest_core, PublishWorkflowRequest,
};
use codeg_lib::acp::delegation::workflow::types::{
    DocumentGateKind, DocumentRef, ManifestDocument, ManifestGate, ManifestNode, ManifestNodeKind,
    ManifestNodeRole, ManifestPhase, ManifestWorkflowState, ResolutionMode, WorkUnitKeyParts,
    MANIFEST_SCHEMA_VERSION, PHASE_DESIGN, PHASE_FINAL, PHASE_PLAN, PHASE_TASKS,
    WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY,
};
use codeg_lib::acp::delegation::workflow::CompletionAttentionCas;
use codeg_lib::acp::delegation::workflow::{
    build_work_unit_key, load_completion_projection, materialize_terminal_completion_txn,
    project_workflow_graph_core, CompletionOutcome, TerminalCompletionInput,
};
use codeg_lib::app_state::AppState;
use codeg_lib::db::entities::delegation_attention_request::AttentionKind;
use codeg_lib::db::entities::delegation_task_run::{self, AdmissionClass, DelegationRunStatus};
use codeg_lib::db::entities::delegation_workflow::{self, CompletionProtocolMode};
use codeg_lib::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
use codeg_lib::models::AgentType;
use codeg_lib::web::auth::COMPLETION_CONTEXT_HEADER;
use codeg_lib::web::event_bridge::EventEmitter;
use codeg_lib::web::router::build_router;
use codeg_lib::web::shutdown::ShutdownSignal;
use sea_orm::{ActiveModelTrait, EntityTrait, Set, TransactionTrait};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const TEST_TOKEN: &str = "completion-attention-token";
const DESIGN_REL_PATH: &str = "docs/superpowers/specs/http-attention-design.md";
const PLAN_REL_PATH: &str = "docs/superpowers/plans/http-attention-plan.md";
const DESIGN_BYTES: &[u8] = b"# Design\n\nHTTP typed attention fixture.\n";

struct CompletionHttpFixture {
    server: TestServer,
    state: Arc<AppState>,
    parent_conversation_id: i32,
    cas: CompletionAttentionCas,
    _workspace: tempfile::TempDir,
    _static_dir: tempfile::TempDir,
}

async fn completion_http_fixture() -> CompletionHttpFixture {
    let workspace = tempfile::tempdir().unwrap();
    let static_dir = tempfile::tempdir().unwrap();
    let workspace_path = workspace.path().to_path_buf();
    let design_path = workspace_path.join(DESIGN_REL_PATH);
    std::fs::create_dir_all(design_path.parent().unwrap()).unwrap();
    std::fs::write(&design_path, DESIGN_BYTES).unwrap();
    let plan_path = workspace_path.join(PLAN_REL_PATH);
    std::fs::create_dir_all(plan_path.parent().unwrap()).unwrap();
    std::fs::write(&plan_path, b"# Plan\n\nHTTP fixture.\n").unwrap();

    let db = fresh_in_memory_db().await;
    let folder = seed_folder(&db, workspace_path.to_str().unwrap()).await;
    let parent = seed_conversation(&db, folder, AgentType::Codex).await;
    let child = seed_conversation(&db, folder, AgentType::Codex).await;
    let author_key = build_work_unit_key(&WorkUnitKeyParts::PlanAuthor {
        rel_plan_path: PLAN_REL_PATH,
        agent_type: "codex",
        profile_id: None,
    })
    .unwrap();
    let published = publish_workflow_manifest_core(
        &db,
        &EventEmitter::Noop,
        parent,
        PublishWorkflowRequest {
            document: completion_manifest(&author_key),
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
        CompletionProtocolMode::V2Enforce
    );

    let task_id = format!("http-attention-{}", uuid::Uuid::new_v4());
    let db_arc = Arc::new(codeg_lib::db::AppDatabase {
        conn: db.conn.clone(),
    });
    RunStore::new(db_arc)
        .admit_gen1_reserving(ReservingRunInsert {
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
            route_fingerprint: Some("http-attention-route".into()),
            launch_snapshot_version: Some("v1".into()),
            mode_id: None,
            config_values_json: Some("{}".into()),
            task_preview: Some("HTTP attention".into()),
            request_fingerprint: Some(format!("fp-{task_id}")),
            admission_class: AdmissionClass::NormalRevision,
            lineage_root_task_id: task_id.clone(),
            work_unit_key: Some(author_key),
            history_only: false,
            replaced_task_id: None,
            replacement_reason: None,
            started_at: Some(Utc::now()),
        })
        .await
        .unwrap();
    let run = delegation_task_run::Entity::find_by_id(&task_id)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    let mut run: delegation_task_run::ActiveModel = run.into();
    run.status = Set(DelegationRunStatus::Completed);
    run.finished_at = Set(Some(Utc::now()));
    run.update(&db.conn).await.unwrap();

    let txn = db.conn.begin().await.unwrap();
    let completion = materialize_terminal_completion_txn(
        &txn,
        TerminalCompletionInput {
            task_id,
            terminal_status: DelegationRunStatus::Completed,
            final_assistant_text: "No explicit conclusion.".into(),
            pre_read_reports: Vec::new(),
            pre_read_artifact: None,
        },
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
    let cas = completion.attention.unwrap();

    let state = Arc::new(AppState::new_for_test(db, workspace_path));
    let router = build_router(
        state.clone(),
        TEST_TOKEN.into(),
        static_dir.path().to_path_buf(),
        Arc::new(ShutdownSignal::new()),
    );
    CompletionHttpFixture {
        server: TestServer::new(router).unwrap(),
        state,
        parent_conversation_id: parent,
        cas,
        _workspace: workspace,
        _static_dir: static_dir,
    }
}

fn completion_manifest(author_key: &str) -> ManifestDocument {
    let design_key = build_work_unit_key(&WorkUnitKeyParts::Design {
        rel_doc_path: DESIGN_REL_PATH,
        agent_type: "codex",
        profile_id: None,
    })
    .unwrap();
    ManifestDocument {
        schema_version: MANIFEST_SCHEMA_VERSION,
        workflow_kind: WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY.into(),
        plan_target_rel_path: PLAN_REL_PATH.into(),
        risk_policy_version: "b2d_task_risk_v1".into(),
        workflow_id: None,
        expected_manifest_revision: None,
        publication_token: "http-attention".into(),
        workflow_state: ManifestWorkflowState::Skeleton,
        design: Some(DocumentRef {
            rel_path: DESIGN_REL_PATH.into(),
            digest: format!("sha256:{:x}", Sha256::digest(DESIGN_BYTES)),
        }),
        plan: None,
        phases: [PHASE_DESIGN, PHASE_PLAN, PHASE_TASKS, PHASE_FINAL]
            .into_iter()
            .map(|id| ManifestPhase {
                id: id.into(),
                kind: Some(id.into()),
                title: None,
            })
            .collect(),
        nodes: vec![
            ManifestNode {
                id: "design-reviewer".into(),
                kind: ManifestNodeKind::WorkUnit,
                phase_id: Some(PHASE_DESIGN.into()),
                role: Some(ManifestNodeRole::Reviewer),
                agent_type: Some("codex".into()),
                profile_id: None,
                task_index: None,
                work_unit_key: Some(design_key),
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
                agent_type: Some("codex".into()),
                profile_id: None,
                task_index: None,
                work_unit_key: Some(author_key.into()),
                deps: Vec::new(),
                required: Some(true),
                node_outcome: None,
                title: None,
            },
        ],
        edges: Vec::new(),
        gates: vec![ManifestGate {
            id: "design".into(),
            reviewer_cohort_node_ids: vec!["design-reviewer".into()],
            required_reviewer_node_ids: vec!["design-reviewer".into()],
            resolution_mode: ResolutionMode::ParentAdjudication,
            gate_kind: Some(DocumentGateKind::Design),
        }],
        task_policies: Vec::new(),
    }
}

#[test]
fn attention_six_field_cas_rejects_every_missing_field() {
    let complete = json!({
        "attention_id": "attention-1",
        "task_id": "task-1",
        "kind": "completion_decision",
        "captured_scope_digest": format!("sha256:{}", "a".repeat(64)),
        "latest_run_id": "task-1",
        "node_id": "plan-reviewer"
    });
    let parsed: CompletionAttentionCas = serde_json::from_value(complete.clone()).unwrap();
    assert_eq!(parsed.kind, AttentionKind::CompletionDecision);

    for field in [
        "attention_id",
        "task_id",
        "kind",
        "captured_scope_digest",
        "latest_run_id",
        "node_id",
    ] {
        let mut missing = complete.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<CompletionAttentionCas>(missing).is_err(),
            "missing CAS field {field} must fail closed"
        );
    }
}

#[test]
fn settle_workflow_gate_v2_only_schema() {
    let catalog: Value = serde_json::from_str(TOOL_SCHEMA_JSON).expect("valid tool schema JSON");
    let settle = catalog
        .as_array()
        .expect("tool catalog array")
        .iter()
        .find(|tool| tool["name"] == "settle_workflow_gate")
        .expect("settle_workflow_gate tool");
    let properties = settle["inputSchema"]["properties"]
        .as_object()
        .expect("settlement properties");

    for removed in ["manifest_revision", "gate_cycle", "outcome", "evidence"] {
        assert!(
            properties.get(removed).is_none(),
            "legacy field {removed} remains"
        );
    }
    for retained in [
        "workflow_id",
        "gate_id",
        "expected_graph_revision",
        "expected_review_round",
        "expected_gate_cycle",
        "expected_outcome",
        "recovery_authorization_id",
        "summary",
    ] {
        assert!(
            properties.get(retained).is_some(),
            "v2 field {retained} missing"
        );
    }

    let request = json!({
        "token": "secret",
        "workflow_id": "workflow-1",
        "gate_id": "design",
        "expected_graph_revision": 4,
        "expected_gate_cycle": 1,
        "expected_outcome": "approved",
        "summary": "settled from platform evidence"
    });
    serde_json::from_value::<BrokerSettleWorkflowRequest>(request.clone())
        .expect("v2 settlement request decodes");
    for (removed, value) in [
        ("manifest_revision", json!(2)),
        ("gate_cycle", json!(1)),
        ("outcome", json!("approved")),
        ("evidence", json!({ "kind": "design" })),
    ] {
        let mut legacy = request.clone();
        legacy[removed] = value;
        let error = serde_json::from_value::<BrokerSettleWorkflowRequest>(legacy)
            .expect_err("legacy settlement property must be rejected");
        assert!(
            error.to_string().contains("unknown field"),
            "legacy field {removed} was not rejected as unknown: {error}"
        );
    }
}

#[test]
fn attention_mutation_routes_and_desktop_commands_are_registered() {
    let router = include_str!("../src/web/router.rs");
    let handlers = include_str!("../src/web/handlers/mod.rs");
    let commands = include_str!("../src/commands/mod.rs");
    let lib = include_str!("../src/lib.rs");

    assert!(handlers.contains("pub mod workflow_completion;"));
    assert!(commands.contains("pub mod workflow_completion;"));
    for operation in [
        "resolve_completion_decision",
        "retry_completion_artifact",
        "resolve_design_self_review",
    ] {
        assert!(
            router.contains(&format!("\"/{operation}\"")),
            "missing authenticated Axum route {operation}"
        );
        assert!(
            lib.contains(&format!("workflow_completion::{operation}")),
            "missing Tauri command registration {operation}"
        );
    }
}

fn error_detail(response: &axum_test::TestResponse) -> String {
    response.json::<Value>()["detail"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

async fn issue_completion_context(fixture: &CompletionHttpFixture) -> String {
    let response = fixture
        .server
        .post("/api/get_workflow_graph_snapshot")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({ "conversationId": fixture.parent_conversation_id }))
        .await;
    response.assert_status_ok();
    response
        .headers()
        .get(COMPLETION_CONTEXT_HEADER)
        .expect("snapshot must issue completion context")
        .to_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn completion_projection_is_identical_across_graph_http_and_mcp_surfaces() {
    let fixture = completion_http_fixture().await;
    let context = issue_completion_context(&fixture).await;

    let pending_graph =
        project_workflow_graph_core(&fixture.state.db, fixture.parent_conversation_id)
            .await
            .unwrap();
    let pending = pending_graph
        .nodes
        .iter()
        .find(|node| node.latest_task_id.as_deref() == Some(fixture.cas.task_id.as_str()))
        .and_then(|node| node.completion.clone())
        .expect("pending completion must be projected");
    assert_eq!(pending.card.state.as_str(), "needs_decision");

    let response = fixture
        .server
        .post("/api/resolve_completion_decision")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .add_header(COMPLETION_CONTEXT_HEADER, context)
        .json(&ResolveCompletionDecisionRequest {
            cas: fixture.cas.clone(),
            outcome: CompletionOutcome::Done,
        })
        .await;
    response.assert_status_ok();

    let direct = project_workflow_graph_core(&fixture.state.db, fixture.parent_conversation_id)
        .await
        .unwrap();
    let http = fixture
        .server
        .post("/api/get_workflow_graph_snapshot")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({ "conversationId": fixture.parent_conversation_id }))
        .await;
    http.assert_status_ok();
    let http: Value = http.json();
    assert_eq!(serde_json::to_value(&direct).unwrap(), http);

    let completion = load_completion_projection(&fixture.state.db.conn, &fixture.cas.task_id)
        .await
        .unwrap()
        .unwrap();
    let graph_completion = direct
        .nodes
        .iter()
        .find(|node| node.latest_task_id.as_deref() == Some(fixture.cas.task_id.as_str()))
        .and_then(|node| node.completion.as_ref())
        .unwrap();
    assert_eq!(graph_completion, &completion);

    let rendered = codeg_lib::acp::delegation::companion::render_status_result(&json!({
        "tasks": [{
            "task_id": fixture.cas.task_id,
            "status": "completed",
            "completion": completion,
        }]
    }));
    assert_eq!(
        rendered["structuredContent"]["tasks"][0]["completion"],
        serde_json::to_value(graph_completion).unwrap()
    );
}

#[test]
fn attention_mutation_dto_rejects_request_asserted_parent_owner() {
    let cas = json!({
        "attention_id": "attention-1",
        "task_id": "task-1",
        "kind": "completion_decision",
        "captured_scope_digest": format!("sha256:{}", "a".repeat(64)),
        "latest_run_id": "task-1",
        "node_id": "plan-author"
    });
    let decision = json!({
        "cas": cas,
        "outcome": "done"
    });
    let retry = json!({
        "cas": decision["cas"].clone()
    });
    let self_review = json!({
        "cas": decision["cas"].clone(),
        "outcome": "approve"
    });
    assert!(serde_json::from_value::<ResolveCompletionDecisionRequest>(decision.clone()).is_ok());
    assert!(serde_json::from_value::<RetryCompletionArtifactRequest>(retry.clone()).is_ok());
    assert!(serde_json::from_value::<ResolveDesignSelfReviewRequest>(self_review.clone()).is_ok());

    let mut decision_with_owner = decision;
    decision_with_owner["parent_conversation_id"] = json!(42);
    assert!(
        serde_json::from_value::<ResolveCompletionDecisionRequest>(decision_with_owner).is_err()
    );
    let mut retry_with_owner = retry;
    retry_with_owner["parent_conversation_id"] = json!(42);
    assert!(serde_json::from_value::<RetryCompletionArtifactRequest>(retry_with_owner).is_err());
    let mut self_review_with_owner = self_review;
    self_review_with_owner["parent_conversation_id"] = json!(42);
    assert!(
        serde_json::from_value::<ResolveDesignSelfReviewRequest>(self_review_with_owner).is_err()
    );
}

#[tokio::test]
async fn attention_authenticated_context_owns_durable_root_across_core_and_http() {
    let matching_fixture = completion_http_fixture().await;
    let matching_token = issue_completion_context(&matching_fixture).await;
    let matching_request = ResolveCompletionDecisionRequest {
        cas: matching_fixture.cas.clone(),
        outcome: CompletionOutcome::Done,
    };
    let global_only = matching_fixture
        .server
        .post("/api/resolve_completion_decision")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&matching_request)
        .await;
    assert_eq!(global_only.status_code(), 403);
    assert_eq!(error_detail(&global_only), "unauthorized");

    let matching_context = matching_fixture
        .state
        .web_server_state
        .completion_authorizations()
        .authenticate(&matching_token)
        .unwrap()
        .authorize_completion_root(matching_fixture.parent_conversation_id)
        .unwrap();
    let resolved = codeg_lib::commands::workflow_completion::resolve_completion_decision_core(
        &matching_fixture.state.db,
        matching_fixture.state.delegation_metrics.as_ref(),
        matching_fixture.state.completion_outbox_dispatcher.as_ref(),
        &matching_context,
        matching_request,
    )
    .await
    .unwrap();
    assert!(!resolved.idempotent_replay);
    let audit = codeg_lib::db::entities::delegation_attention_request::Entity::find_by_id(
        &matching_fixture.cas.attention_id,
    )
    .one(&matching_fixture.state.db.conn)
    .await
    .unwrap()
    .unwrap()
    .resolution_json
    .unwrap();
    assert!(audit.contains(&format!(
        "web_completion_root:{}",
        matching_fixture.parent_conversation_id
    )));
    assert!(!audit.contains(&matching_token));
    assert!(!audit.contains(&format!("{:x}", Sha256::digest(TEST_TOKEN.as_bytes()))));

    let foreign_fixture = completion_http_fixture().await;
    let foreign_request = ResolveCompletionDecisionRequest {
        cas: foreign_fixture.cas.clone(),
        outcome: CompletionOutcome::Done,
    };
    let foreign_token = foreign_fixture
        .state
        .web_server_state
        .completion_authorizations()
        .issue(foreign_fixture.parent_conversation_id + 1);
    let foreign_context = foreign_fixture
        .state
        .web_server_state
        .completion_authorizations()
        .authenticate(&foreign_token)
        .unwrap()
        .authorize_completion_root(foreign_fixture.parent_conversation_id + 1)
        .unwrap();
    let core_foreign = codeg_lib::commands::workflow_completion::resolve_completion_decision_core(
        &foreign_fixture.state.db,
        foreign_fixture.state.delegation_metrics.as_ref(),
        foreign_fixture.state.completion_outbox_dispatcher.as_ref(),
        &foreign_context,
        foreign_request.clone(),
    )
    .await
    .unwrap_err();
    let core_foreign = serde_json::to_value(core_foreign).unwrap();

    let http_foreign = foreign_fixture
        .server
        .post("/api/resolve_completion_decision")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .add_header(COMPLETION_CONTEXT_HEADER, foreign_token)
        .json(&foreign_request)
        .await;
    assert_eq!(http_foreign.status_code(), 403);
    assert_eq!(
        error_detail(&http_foreign),
        core_foreign["detail"].as_str().unwrap()
    );
}

#[tokio::test]
async fn attention_authenticated_http_matches_core_for_cas_replay_and_conflict() {
    let fixture = completion_http_fixture().await;
    let completion_context = issue_completion_context(&fixture).await;
    let request = ResolveCompletionDecisionRequest {
        cas: fixture.cas.clone(),
        outcome: CompletionOutcome::Done,
    };

    let free_form = fixture
        .state
        .delegation_broker
        .reply_to_delegation(
            "parent-connection",
            Some(fixture.parent_conversation_id),
            &fixture.cas.attention_id,
            "done",
        )
        .await;
    assert!(matches!(
        free_form,
        DelegationReplyResult::Rejected { code, .. } if code == "attention_kind_mismatch"
    ));

    let unauthenticated = fixture
        .server
        .post("/api/resolve_completion_decision")
        .json(&request)
        .await;
    assert_eq!(unauthenticated.status_code(), 401);

    let mut stale_request = request.clone();
    stale_request.cas.node_id.push_str("-stale");
    let stale = fixture
        .server
        .post("/api/resolve_completion_decision")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .add_header(COMPLETION_CONTEXT_HEADER, &completion_context)
        .json(&stale_request)
        .await;
    assert_eq!(stale.status_code(), 409);
    assert_eq!(error_detail(&stale), "completion_decision_superseded");

    let invalid_role = fixture
        .server
        .post("/api/resolve_completion_decision")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .add_header(COMPLETION_CONTEXT_HEADER, &completion_context)
        .json(&ResolveCompletionDecisionRequest {
            outcome: CompletionOutcome::Approve,
            ..request.clone()
        })
        .await;
    assert_eq!(invalid_role.status_code(), 400);
    assert_eq!(
        error_detail(&invalid_role),
        "completion_outcome_role_mismatch"
    );

    let mut missing = serde_json::to_value(&request).unwrap();
    missing["cas"]
        .as_object_mut()
        .unwrap()
        .remove("latest_run_id");
    let missing = fixture
        .server
        .post("/api/resolve_completion_decision")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .add_header(COMPLETION_CONTEXT_HEADER, &completion_context)
        .json(&missing)
        .await;
    assert_eq!(missing.status_code(), 422);

    let first = fixture
        .server
        .post("/api/resolve_completion_decision")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .add_header(COMPLETION_CONTEXT_HEADER, &completion_context)
        .json(&request)
        .await;
    first.assert_status_ok();
    let first: CompletionMutationResult = first.json();
    assert!(!first.idempotent_replay);

    let replay = fixture
        .server
        .post("/api/resolve_completion_decision")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .add_header(COMPLETION_CONTEXT_HEADER, &completion_context)
        .json(&request)
        .await;
    replay.assert_status_ok();
    let replay: CompletionMutationResult = replay.json();
    assert!(replay.idempotent_replay);
    assert_eq!(first.graph_revision, replay.graph_revision);

    let conflict = fixture
        .server
        .post("/api/resolve_completion_decision")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .add_header(COMPLETION_CONTEXT_HEADER, &completion_context)
        .json(&ResolveCompletionDecisionRequest {
            outcome: CompletionOutcome::Blocked,
            ..request
        })
        .await;
    assert_eq!(conflict.status_code(), 409);
    assert_eq!(error_detail(&conflict), "completion_decision_conflict");
}

#[test]
fn legacy_restart_surface_is_absent() {
    let production_sources = [
        ("tool schema", TOOL_SCHEMA_JSON),
        (
            "companion",
            include_str!("../src/acp/delegation/companion.rs"),
        ),
        (
            "transport",
            include_str!("../src/acp/delegation/transport.rs"),
        ),
        (
            "listener",
            include_str!("../src/acp/delegation/listener.rs"),
        ),
        ("broker", include_str!("../src/acp/delegation/broker.rs")),
        (
            "workflow types",
            include_str!("../src/acp/delegation/workflow/types.rs"),
        ),
        (
            "workflow errors",
            include_str!("../src/acp/delegation/workflow/error.rs"),
        ),
        ("ACP errors", include_str!("../src/acp/error.rs")),
        ("app errors", include_str!("../src/app_error.rs")),
        (
            "commands",
            include_str!("../src/commands/workflow_completion.rs"),
        ),
        (
            "web handler",
            include_str!("../src/web/handlers/workflow_completion.rs"),
        ),
        ("web router", include_str!("../src/web/router.rs")),
        ("Tauri registration", include_str!("../src/lib.rs")),
    ];
    let forbidden = [
        ["restart_legacy_", "workflow"].concat(),
        ["LegacyWorkflowRestart", "Projection"].concat(),
        ["LegacyCompletionProtocol", "Restart"].concat(),
        ["legacy_completion_protocol_", "restart_required"].concat(),
        ["legacy_completion_protocol_", "restart_invalid"].concat(),
        ["legacy_completion_protocol_", "restart_not_required"].concat(),
        ["successor_conversation_", "id"].concat(),
        ["capture_original_request_", "context"].concat(),
    ];

    for (surface, source) in production_sources {
        for removed in &forbidden {
            assert!(
                !source.contains(removed),
                "legacy restart surface {removed} remains in {surface}"
            );
        }
    }
}
