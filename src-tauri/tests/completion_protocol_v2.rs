use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use axum_test::TestServer;
use codeg_lib::acp::delegation::broker::{
    compare_completion_shadow_outcome, is_completion_format_repair_prompt,
};
use codeg_lib::acp::delegation::companion::{
    CompanionContext, CompanionFeatures, TOOL_SCHEMA_JSON,
};
use codeg_lib::acp::delegation::metrics::{
    CompletionContinuationReason, CompletionFinalMetricState, CompletionRestartOutcome,
    CompletionShadowDifference, DelegationMetrics,
};
use codeg_lib::acp::delegation::run_store::{ReservingRunInsert, RunStore};
use codeg_lib::acp::delegation::transport::CompanionRole;
use codeg_lib::acp::delegation::types::{CompletionMutationContext, RestartLegacyWorkflowRequest};
use codeg_lib::acp::delegation::workflow::{
    build_work_unit_key, capture_original_request_context, evaluate_rollout_window,
    get_workflow_state_core, guard_final_delivery_core, inject_legacy_restart_header_failure_once,
    materialize_terminal_completion_txn, project_workflow_graph_core,
    publish_workflow_manifest_core, publish_workflow_manifest_with_selection_core,
    resolve_completion_decision_txn, restart_legacy_workflow_core,
    restart_legacy_workflow_if_enforced, select_completion_protocol, CompletionCardV2,
    CompletionIntent, CompletionIntentSource, CompletionOutcome, CompletionProtocolRolloutConfig,
    CompletionProtocolSelection, CompletionResolution, CompletionRole, DocumentRef,
    FinalDeliveryGuardRequest, FinalDeliveryGuardResult, ManifestDocument, ManifestNode,
    ManifestNodeKind, ManifestNodeRole, ManifestPhase, ManifestWorkflowState, PlanReviewChangeV2,
    PlanReviewNextAction, ProfileCompletionWindow, PublishWorkflowRequest, RolloutDecision,
    TerminalCompletionInput, ValidatedReportCandidate, WorkUnitKeyParts, MANIFEST_SCHEMA_VERSION,
    PHASE_DESIGN, PHASE_FINAL, PHASE_PLAN, WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY,
};
use codeg_lib::acp::error::AcpError;
use codeg_lib::acp::manager::ConnectionManager;
use codeg_lib::acp::types::PromptInputBlock;
use codeg_lib::app_state::AppState;
use codeg_lib::commands::workflow_completion::restart_legacy_workflow_authenticated_core;
use codeg_lib::db::entities::{
    auto_title_job, delegation_attention_request, delegation_completion_tool_intent,
    delegation_task_run, delegation_workflow, delegation_workflow_gate_settlement,
    delegation_workflow_gate_state, delegation_workflow_manifest_revision,
    delegation_workflow_node_binding, delegation_workflow_restart_context,
    delegation_workflow_run_binding,
};
use codeg_lib::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
use codeg_lib::models::AgentType;
use codeg_lib::web::event_bridge::EventEmitter;
use codeg_lib::web::router::build_router;
use codeg_lib::web::shutdown::ShutdownSignal;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, Set, TransactionTrait,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

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
    format_repair_run_count: usize,
    card_reemit_prompt_count: usize,
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
    RunStore::new(Arc::new(codeg_lib::db::AppDatabase {
        conn: db.conn.clone(),
    }))
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

    if matches!(case, CapabilityCase::ToolCompleteWork) {
        delegation_completion_tool_intent::ActiveModel {
            intent_id: Set(format!("intent-{task_id}")),
            task_id: Set(task_id.clone()),
            child_tool_call_id: Set(format!("call-{task_id}")),
            accepted_ordinal: Set(1),
            outcome: Set(CompletionOutcome::Done.as_str().into()),
            summary: Set(Some("tool completion".into())),
            report_hint: Set(None),
            request_digest: Set(format!("digest-{task_id}")),
            created_at: Set(chrono::Utc::now()),
        }
        .insert(&db.conn)
        .await
        .unwrap();
    }

    let (final_assistant_text, pre_read_reports) = match case {
        CapabilityCase::ToolCompleteWork => ("Tool completion submitted.".into(), Vec::new()),
        CapabilityCase::TerminalConclusionOnly => {
            ("Implementation complete.\n\nConclusion: done".into(), Vec::new())
        }
        CapabilityCase::ReportConclusionOnly => (
            "See [the report](reports/task-18.md).".into(),
            vec![ValidatedReportCandidate {
                path: "reports/task-18.md".into(),
                contents: "# Task 18 report\n\nConclusion: done\n".into(),
                summary: Some("report completion".into()),
            }],
        ),
        CapabilityCase::AmbiguousThenUserAdjudication => (
            "Implemented the requested changes without an explicit conclusion.".into(),
            Vec::new(),
        ),
        CapabilityCase::ObsoleteCardPlusNaturalConclusion => (
            "```json\n{\"kind\":\"author\",\"status\":\"done\",\"plan_digest\":\"model\"}\n```\n\nConclusion: done"
                .into(),
            Vec::new(),
        ),
    };
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
    if matches!(case, CapabilityCase::AmbiguousThenUserAdjudication) {
        resolve_completion_decision_txn(
            &db,
            parent,
            terminal.attention.expect("ambiguous completion decision"),
            CompletionOutcome::Done,
            "application_user",
        )
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
    let format_repair_run_count = runs
        .iter()
        .filter(|run| run.task_preview.as_deref() == Some("CARD RE-EMIT ONLY"))
        .count();

    let static_dir = tempfile::tempdir().unwrap();
    let state = Arc::new(AppState::new_for_test(db, workspace_path));
    let server = TestServer::new(build_router(
        state.clone(),
        "task-18-token".into(),
        static_dir.path().to_path_buf(),
        Arc::new(ShutdownSignal::new()),
    ))
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
    let rendered = codeg_lib::acp::delegation::companion::render_status_result(&json!({
        "tasks": [{
            "task_id": task_id,
            "status": "completed",
            "completion": desktop,
        }]
    }));
    let mcp_completion = rendered["structuredContent"]["tasks"][0]["completion"]["card"].clone();
    let desktop_completion = serde_json::to_value(&desktop.card).unwrap();

    CapabilityResult {
        child_run_count: runs.len() as u64,
        card_summary_json: stored_run.card_summary_json,
        completion: desktop.card,
        desktop_completion,
        server_completion,
        mcp_completion,
        format_repair_run_count,
        card_reemit_prompt_count: format_repair_run_count,
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
    document
}

async fn run_final_drift_fixture() -> (FinalDeliveryGuardResult, String, i64) {
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
    let reviewed_head = git_fixture(repo.path(), &["rev-parse", "HEAD"]);

    let db = fresh_in_memory_db().await;
    let folder = seed_folder(&db, repo.path().to_str().unwrap()).await;
    let parent = seed_conversation(&db, folder, AgentType::Codex).await;
    let child = seed_conversation(&db, folder, AgentType::Codex).await;
    let published = publish_workflow_manifest_core(
        &db,
        &EventEmitter::Noop,
        parent,
        PublishWorkflowRequest {
            document: final_review_skeleton("task-18-final-drift"),
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
    workflow.completion_protocol_mode = Set(delegation_workflow::CompletionProtocolMode::V2Enforce);
    workflow.update(&db.conn).await.unwrap();
    delegation_workflow_gate_state::ActiveModel {
        workflow_id: Set(published.workflow_id.clone()),
        gate_id: Set("final".into()),
        gate_lineage: Set("sha256:task-18-final-lineage".into()),
        current_review_round: Set(1),
        selected_node_ids_json: Set("[\"final-reviewer\"]".into()),
    }
    .insert(&db.conn)
    .await
    .unwrap();

    let now = chrono::Utc::now();
    let task_id = "task-18-passing-final-review";
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
        workspace_path: Set(Some(repo.path().to_string_lossy().into_owned())),
        route_fingerprint: Set(None),
        launch_snapshot_version: Set(None),
        mode_id: Set(None),
        config_values_json: Set(None),
        task_preview: Set(Some("Task 18 Final review".into())),
        request_fingerprint: Set(None),
        admission_class: Set(delegation_task_run::AdmissionClass::NormalRevision),
        reached_running_at: Set(Some(now)),
        lineage_root_task_id: Set(task_id.into()),
        work_unit_key: Set(None),
        legacy_parent_tool_use_id: Set(None),
        history_only: Set(false),
        status: Set(delegation_task_run::DelegationRunStatus::Completed),
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
    .unwrap();
    delegation_workflow_run_binding::ActiveModel {
        task_id: Set(task_id.into()),
        workflow_id: Set(published.workflow_id.clone()),
        node_id: Set("final-reviewer".into()),
        gate_id: Set(Some("final".into())),
        gate_cycle: Set(Some(1)),
        manifest_revision: Set(published.manifest_revision as i64),
        content_fingerprint: Set(None),
        artifact_digest: Set(Some(reviewed_head)),
        reviewed_task_id: Set(None),
        reviewed_implementer_generation: Set(None),
        lineage_ordinal: Set(1),
        summary_validated: Set(true),
        gate_lineage: Set(Some("sha256:task-18-final-lineage".into())),
        review_round: Set(Some(1)),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(&db.conn)
    .await
    .unwrap();

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

    let guarded = guard_final_delivery_core(
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
    let gate = delegation_workflow_gate_state::Entity::find_by_id((
        published.workflow_id,
        "final".to_string(),
    ))
    .one(&db.conn)
    .await
    .unwrap()
    .unwrap();
    (guarded, gate.gate_id, gate.current_review_round)
}

#[tokio::test]
async fn session_2889_and_final_drift_have_no_format_repair_escape() {
    let session = run_capability_case(CapabilityCase::ObsoleteCardPlusNaturalConclusion).await;
    assert_eq!(session.format_repair_run_count, 0);
    assert_eq!(session.card_reemit_prompt_count, 0);
    assert_eq!(session.child_run_count, 1);

    let (final_delivery, current_gate, review_round) = run_final_drift_fixture().await;
    assert_eq!(
        final_delivery.diagnostic_code(),
        Some("final_artifact_drift")
    );
    assert!(final_delivery.reopened().is_some());
    assert_eq!(current_gate, "final");
    assert_eq!(review_round, 2);
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
async fn legacy_prompt_restart_fences_desktop_and_server_plain_text_before_source_mutation() {
    let (db, parent, source_workflow_id) = legacy_source().await;
    let before = source_fingerprint(&db, parent, &source_workflow_id).await;
    let folder_id = codeg_lib::db::entities::conversation::Entity::find_by_id(parent)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap()
        .folder_id;
    let metrics = std::sync::Arc::new(DelegationMetrics::default());
    let rollout = CompletionProtocolRolloutConfig {
        default_mode: delegation_workflow::CompletionProtocolMode::V2Enforce,
        ..Default::default()
    };
    let rollout = std::sync::Arc::new(rollout);
    let manager = ConnectionManager::new();
    manager.install_completion_protocol_runtime(rollout, metrics);
    manager
        .insert_test_connection(
            "legacy-root",
            AgentType::Codex,
            Some(std::path::PathBuf::from("/tmp/task-15-legacy-restart")),
            EventEmitter::Noop,
        )
        .await;
    let state = manager.get_state("legacy-root").await.unwrap();
    {
        let mut state = state.write().await;
        state.conversation_id = Some(parent);
    }

    let error = manager
        .send_prompt_linked_with_message_id(
            &db,
            "legacy-root",
            vec![PromptInputBlock::Text {
                text: "continue without workflow tools".into(),
            }],
            Some(folder_id),
            Some(parent),
            None,
            Some("plain-text-resume".into()),
            None,
        )
        .await
        .expect_err("legacy prompt must be redirected before admission");
    let successor_conversation_id = match error {
        AcpError::LegacyCompletionProtocolRestart {
            successor_conversation_id,
        } => successor_conversation_id,
        other => panic!("expected typed legacy restart, got {other:?}"),
    };

    assert_ne!(successor_conversation_id, parent);
    assert_eq!(
        source_fingerprint(&db, parent, &source_workflow_id).await,
        before
    );
    assert!(!state.read().await.turn_in_flight);
    assert_eq!(
        delegation_workflow::Entity::find()
            .filter(delegation_workflow::Column::LegacySourceWorkflowId.eq(source_workflow_id))
            .count(&db.conn)
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn legacy_restart_upgrade_recovers_first_request_before_enforce_resume() {
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
        .expect_err("first enforce resume must redirect to a recovered successor");
    let successor_conversation_id = match error {
        AcpError::LegacyCompletionProtocolRestart {
            successor_conversation_id,
        } => successor_conversation_id,
        other => panic!("expected typed legacy restart, got {other:?}"),
    };

    assert_eq!(
        source_fingerprint(&db, parent, &published.workflow_id).await,
        before,
        "upgrade recovery must not mutate the source workflow or conversation"
    );
    assert!(!state.read().await.turn_in_flight);
    let context =
        delegation_workflow_restart_context::Entity::find_by_id(successor_conversation_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(context.original_conversation_id, parent);
    assert_eq!(context.original_request_text, original_request);
    assert_ne!(context.original_request_id, "upgrade-resume-turn");
    let successor = delegation_workflow::Entity::find()
        .filter(delegation_workflow::Column::LegacySourceWorkflowId.eq(&published.workflow_id))
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(successor.parent_conversation_id, successor_conversation_id);
    assert_eq!(
        delegation_workflow_manifest_revision::Entity::find()
            .filter(
                delegation_workflow_manifest_revision::Column::WorkflowId
                    .eq(&successor.workflow_id),
            )
            .count(&db.conn)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        delegation_task_run::Entity::find()
            .filter(
                delegation_task_run::Column::ParentConversationId.eq(successor_conversation_id),
            )
            .count(&db.conn)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        delegation_workflow_run_binding::Entity::find()
            .filter(delegation_workflow_run_binding::Column::WorkflowId.eq(&successor.workflow_id),)
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
async fn rollout_mode_is_frozen_per_workflow() {
    let db = fresh_in_memory_db().await;
    let folder = seed_folder(&db, "/tmp/task-15-frozen-rollout").await;
    let parent = seed_conversation(&db, folder, AgentType::Codex).await;
    let mut config = CompletionProtocolRolloutConfig {
        default_mode: delegation_workflow::CompletionProtocolMode::V2Shadow,
        ..Default::default()
    };
    let selection = select_completion_protocol("codex", Some("canary"), &config);
    let published = publish_workflow_manifest_with_selection_core(
        &db,
        &EventEmitter::Noop,
        parent,
        PublishWorkflowRequest {
            document: skeleton("task-15-shadow"),
        },
        selection,
    )
    .await
    .unwrap();

    config.default_mode = delegation_workflow::CompletionProtocolMode::V1;
    let row = delegation_workflow::Entity::find_by_id(published.workflow_id)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.completion_protocol_version, 1);
    assert_eq!(
        row.completion_protocol_mode,
        delegation_workflow::CompletionProtocolMode::V2Shadow
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
    publish_workflow_manifest_with_selection_core(
        &db,
        &EventEmitter::Noop,
        parent,
        PublishWorkflowRequest {
            document: skeleton("task-15-stored-shadow-source"),
        },
        CompletionProtocolSelection {
            version: 1,
            mode: delegation_workflow::CompletionProtocolMode::V2Shadow,
            source:
                codeg_lib::acp::delegation::workflow::CompletionProtocolSelectionSource::Default,
        },
    )
    .await
    .unwrap();

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
