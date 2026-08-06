use codeg_lib::acp::delegation::companion::{
    CompanionContext, CompanionFeatures, TOOL_SCHEMA_JSON,
};
use codeg_lib::acp::delegation::metrics::{
    CompletionRestartOutcome, CompletionShadowDifference, DelegationMetrics,
};
use codeg_lib::acp::delegation::transport::CompanionRole;
use codeg_lib::acp::delegation::workflow::{
    build_work_unit_key, evaluate_rollout_window, get_workflow_state_core,
    inject_legacy_restart_header_failure_once, project_workflow_graph_core,
    publish_workflow_manifest_core, publish_workflow_manifest_with_selection_core,
    restart_legacy_workflow_core, select_completion_protocol, CompletionIntentSource,
    CompletionProtocolRolloutConfig, CompletionProtocolSelection, CompletionRole, ManifestDocument,
    ManifestNode, ManifestNodeKind, ManifestNodeRole, ManifestPhase, ManifestWorkflowState,
    ProfileCompletionWindow, PublishWorkflowRequest, RolloutDecision, WorkUnitKeyParts,
    MANIFEST_SCHEMA_VERSION, PHASE_DESIGN, PHASE_PLAN, WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY,
};
use codeg_lib::db::entities::{
    delegation_attention_request, delegation_task_run, delegation_workflow,
    delegation_workflow_gate_settlement, delegation_workflow_manifest_revision,
    delegation_workflow_run_binding,
};
use codeg_lib::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
use codeg_lib::models::AgentType;
use codeg_lib::web::event_bridge::EventEmitter;
use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};
use serde_json::Value;

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

async fn legacy_source() -> (codeg_lib::db::AppDatabase, i32, String) {
    let db = fresh_in_memory_db().await;
    let folder = seed_folder(&db, "/tmp/task-15-legacy-restart").await;
    let parent = seed_conversation(&db, folder, AgentType::Codex).await;
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
async fn rollout_mode_is_frozen_per_workflow() {
    let db = fresh_in_memory_db().await;
    let folder = seed_folder(&db, "/tmp/task-15-frozen-rollout").await;
    let parent = seed_conversation(&db, folder, AgentType::Codex).await;
    let mut config = CompletionProtocolRolloutConfig::default();
    config.default_mode = delegation_workflow::CompletionProtocolMode::V2Shadow;
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
    assert!(!metrics
        .record_format_repair_child_run(delegation_workflow::CompletionProtocolMode::V2Enforce,));
    assert!(
        !metrics.record_card_reemit_prompt(delegation_workflow::CompletionProtocolMode::V2Enforce,)
    );

    let snapshot = metrics.snapshot().completion_protocol;
    assert_eq!(snapshot.creation_modes["v2_shadow"], 1);
    assert_eq!(snapshot.restart_outcomes["created"], 1);
    assert_eq!(snapshot.shadow_differences["needs_decision"], 1);
    assert_eq!(snapshot.format_only_child_runs, 0);
    assert_eq!(snapshot.card_reemit_prompts, 0);
    assert_eq!(snapshot.natural_language_fallback_count, 1);
}
