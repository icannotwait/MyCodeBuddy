use std::sync::Arc;

use axum_test::TestServer;
use chrono::Utc;
use codeg_lib::acp::delegation::run_store::{ReservingRunInsert, RunStore};
use codeg_lib::acp::delegation::types::{
    CompletionMutationResult, DelegationReplyResult, ResolveCompletionDecisionRequest,
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
    build_work_unit_key, materialize_terminal_completion_txn, CompletionOutcome,
    TerminalCompletionInput,
};
use codeg_lib::app_state::AppState;
use codeg_lib::db::entities::delegation_attention_request::AttentionKind;
use codeg_lib::db::entities::delegation_task_run::{self, AdmissionClass, DelegationRunStatus};
use codeg_lib::db::entities::delegation_workflow::{self, CompletionProtocolMode};
use codeg_lib::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
use codeg_lib::models::AgentType;
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
    let mut workflow: delegation_workflow::ActiveModel = workflow.into();
    workflow.completion_protocol_version = Set(2);
    workflow.completion_protocol_mode = Set(CompletionProtocolMode::V2Enforce);
    workflow.update(&db.conn).await.unwrap();

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

#[tokio::test]
async fn attention_authenticated_http_matches_core_for_cas_replay_and_conflict() {
    let fixture = completion_http_fixture().await;
    let request = ResolveCompletionDecisionRequest {
        parent_conversation_id: fixture.parent_conversation_id,
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

    let foreign_request = ResolveCompletionDecisionRequest {
        parent_conversation_id: fixture.parent_conversation_id + 1,
        ..request.clone()
    };
    let core_foreign = codeg_lib::commands::workflow_completion::resolve_completion_decision_core(
        &fixture.state.db,
        fixture.state.completion_outbox_dispatcher.as_ref(),
        foreign_request.clone(),
    )
    .await
    .unwrap_err();
    let core_foreign = serde_json::to_value(core_foreign).unwrap();
    let http_foreign = fixture
        .server
        .post("/api/resolve_completion_decision")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&foreign_request)
        .await;
    assert_eq!(http_foreign.status_code(), 403);
    assert_eq!(
        error_detail(&http_foreign),
        core_foreign["detail"].as_str().unwrap()
    );

    let mut stale_request = request.clone();
    stale_request.cas.node_id.push_str("-stale");
    let stale = fixture
        .server
        .post("/api/resolve_completion_decision")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&stale_request)
        .await;
    assert_eq!(stale.status_code(), 409);
    assert_eq!(error_detail(&stale), "completion_decision_superseded");

    let invalid_role = fixture
        .server
        .post("/api/resolve_completion_decision")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
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
        .json(&missing)
        .await;
    assert_eq!(missing.status_code(), 422);

    let first = fixture
        .server
        .post("/api/resolve_completion_decision")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&request)
        .await;
    first.assert_status_ok();
    let first: CompletionMutationResult = first.json();
    assert!(!first.idempotent_replay);

    let replay = fixture
        .server
        .post("/api/resolve_completion_decision")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
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
        .json(&ResolveCompletionDecisionRequest {
            outcome: CompletionOutcome::Blocked,
            ..request
        })
        .await;
    assert_eq!(conflict.status_code(), 409);
    assert_eq!(error_detail(&conflict), "completion_decision_conflict");
}
