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
use codeg_lib::acp::delegation::transport::CompanionRole;
use codeg_lib::acp::delegation::types::{CompletionMutationContext, RestartLegacyWorkflowRequest};
use codeg_lib::acp::delegation::workflow::{
    build_work_unit_key, capture_original_request_context, evaluate_rollout_window,
    get_workflow_state_core, inject_legacy_restart_header_failure_once,
    project_workflow_graph_core, publish_workflow_manifest_core,
    publish_workflow_manifest_with_selection_core, restart_legacy_workflow_core,
    restart_legacy_workflow_if_enforced, select_completion_protocol, CompletionIntent,
    CompletionIntentSource, CompletionOutcome, CompletionProtocolRolloutConfig,
    CompletionProtocolSelection, CompletionResolution, CompletionRole, ManifestDocument,
    ManifestNode, ManifestNodeKind, ManifestNodeRole, ManifestPhase, ManifestWorkflowState,
    PlanReviewChangeV2, PlanReviewNextAction, ProfileCompletionWindow, PublishWorkflowRequest,
    RolloutDecision, WorkUnitKeyParts, MANIFEST_SCHEMA_VERSION, PHASE_DESIGN, PHASE_PLAN,
    WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY,
};
use codeg_lib::acp::error::AcpError;
use codeg_lib::acp::manager::ConnectionManager;
use codeg_lib::acp::types::PromptInputBlock;
use codeg_lib::commands::workflow_completion::restart_legacy_workflow_authenticated_core;
use codeg_lib::db::entities::{
    delegation_attention_request, delegation_task_run, delegation_workflow,
    delegation_workflow_gate_settlement, delegation_workflow_manifest_revision,
    delegation_workflow_node_binding, delegation_workflow_run_binding,
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
async fn legacy_restart_context_preserves_request_and_non_default_author_without_auto_title() {
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

    let mut document = skeleton("task-15-grok-profile-source");
    let author = document.nodes.first_mut().unwrap();
    author.agent_type = Some("grok".into());
    author.profile_id = Some("review-canary".into());
    author.work_unit_key = Some(
        build_work_unit_key(&WorkUnitKeyParts::PlanAuthor {
            rel_plan_path: &document.plan_target_rel_path,
            agent_type: "grok",
            profile_id: Some("review-canary"),
        })
        .unwrap(),
    );
    publish_workflow_manifest_core(
        &db,
        &EventEmitter::Noop,
        parent,
        PublishWorkflowRequest { document },
    )
    .await
    .unwrap();

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
    assert_eq!(successor_author.agent_type, "grok");
    assert_eq!(
        successor_author.profile_id.as_deref(),
        Some("review-canary")
    );
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
    let mut rollout = CompletionProtocolRolloutConfig::default();
    rollout.default_mode = delegation_workflow::CompletionProtocolMode::V2Enforce;
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

    let mut enforce = CompletionProtocolRolloutConfig::default();
    enforce.default_mode = delegation_workflow::CompletionProtocolMode::V2Enforce;
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
