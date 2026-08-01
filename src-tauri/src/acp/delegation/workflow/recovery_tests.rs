//! End-to-end acceptance fixtures for authorized delegation recovery.

use super::*;
use crate::acp::delegation::broker::{DbDepthLookup, DelegationBroker, DelegationConfig};
use crate::acp::delegation::recovery_policy::{
    decide_delegation_recovery, RecoveryConfirmation, RecoveryDecision, RecoveryRailSnapshot,
    RequestedRecoveryOperation,
};
use crate::acp::delegation::run_store::{ReservingRunInsert, RunStore};
use crate::acp::delegation::spawner::{mock::MockSpawner, ConnectionSpawner, SpawnerError};
use crate::acp::delegation::store::{DbDelegationTaskStore, DelegationTaskStore};
use crate::acp::delegation::types::{ContinueDelegationRequest, DelegationRecoveryProjection};
use crate::acp::question::{QuestionAnsweredItem, QuestionOutcome};
use crate::acp::recovery_authorization::{
    DelegationAuthorizationIdentity, PreparedAuthorization, RecoveryAllowedAction,
    RecoveryAuthorizationService, RecoveryAuthorizationStore, RecoveryChallenge,
    RecoverySubjectKind, RECOVERY_APPROVE_LABEL,
};
use crate::db::entities::delegation_task_run::{AdmissionClass, DelegationRunStatus};
use crate::db::entities::delegation_workflow_gate_settlement::GateSettlementOutcome;
use crate::db::entities::{
    delegation_task_run, delegation_workflow, delegation_workflow_manifest_revision,
    delegation_workflow_node_binding, delegation_workflow_run_binding, recovery_authorization,
};
use crate::db::service::conversation_service;
use crate::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
use crate::models::agent::AgentType;
use crate::web::event_bridge::{EventEmitter, WebEventBroadcaster};
use chrono::{Duration, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, IntoActiveModel, PaginatorTrait,
    QueryFilter, QueryOrder, Set, TransactionTrait,
};
use std::sync::Arc;

#[tokio::test]
async fn session_2566_blocked_workflow_recovers_in_place_to_task_one_admission() {
    // workflow_id = afd89cd7-5df0-49d9-8a40-1d2c95791cbd
    // revision/header = 8/blocked
    // Plan digest = sha256:77fca1481d57395b3b7fe090be2d116e647f6275e303895b0b88e7ad4428d4b5
    let db = fresh_in_memory_db().await;
    let parent = seed_conversation(
        &db,
        seed_folder(&db, "/tmp/session-2566-parent").await,
        AgentType::Codex,
    )
    .await;
    let emitter = EventEmitter::test_web_only(Arc::new(WebEventBroadcaster::new()));
    let published = publish_workflow_manifest_core(
        &db,
        &emitter,
        parent,
        PublishWorkflowRequest {
            document: session_2566_document(),
        },
    )
    .await
    .expect("publish session-2566 fixture");
    let workflow_id = rename_workflow_id(&db, &published.workflow_id).await;
    assert_eq!(workflow_id, SESSION_2566_WORKFLOW_ID);

    let author_task_id = "session-2566-author";
    insert_plan_run(
        &db,
        parent,
        &workflow_id,
        "plan-author",
        author_task_id,
        "codex",
        None,
        true,
        serde_json::json!({
            "kind": "author",
            "status": "done",
            "summary": "Plan artifact completed",
            "plan_digest": SESSION_2566_PLAN_DIGEST,
            "report_file": "reports/session-2566-author.md",
        }),
    )
    .await;
    for (node_id, task_id, agent_type) in [
        ("plan-reviewer-1", "session-2566-review-codex", "codex"),
        ("plan-reviewer-2", "session-2566-review-grok", "grok"),
    ] {
        insert_plan_run(
            &db,
            parent,
            &workflow_id,
            node_id,
            task_id,
            agent_type,
            Some(author_task_id),
            true,
            serde_json::json!({
                "kind": "review",
                "verdict": "approve",
                "critical": 0,
                "important": 0,
                "minor": 0,
                "summary": "Plan review completed",
                "report_file": format!("reports/{task_id}.md"),
            }),
        )
        .await;
    }

    let settled = settle_workflow_gate_core(
        &db,
        &emitter,
        parent,
        SettleWorkflowRequest {
            workflow_id: workflow_id.clone(),
            manifest_revision: 1,
            gate_id: "plan".into(),
            expected_graph_revision: published.graph_revision,
            gate_cycle: 1,
            outcome: GateSettlementOutcome::Approved,
            evidence: SettleGateEvidence::Plan(PlanReviewRoundSubmission {
                scope: PlanReviewScope::Full,
                revision_kind: PlanRevisionKind::Initial,
                scope_reason: "reconstructed session-2566 approval".into(),
                covered_author_task_id: "session-2566-author".into(),
                covered_plan_digest: SESSION_2566_PLAN_DIGEST.into(),
                required_reviewer_node_ids: vec![
                    "plan-reviewer-1".into(),
                    "plan-reviewer-2".into(),
                ],
                finding_updates: vec![],
                lineage_reset_reason: None,
            }),
            summary: "cycle one approved".into(),
            recovery_authorization_id: None,
        },
    )
    .await
    .expect("current Plan evidence approves cycle one");

    assert_eq!(settled.outcome, GateSettlementOutcome::Approved);
    assert_eq!(settled.critical_count, 0);
    assert_eq!(settled.important_count, 0);

    append_session_revisions(&db, &workflow_id).await;
    let blocked_header = load_header(&db, &workflow_id).await;
    assert_eq!(blocked_header.active_manifest_revision, 8);
    assert_eq!(
        blocked_header.workflow_state,
        crate::db::entities::delegation_workflow::WorkflowState::Blocked
    );
    let retired_before = insert_retired_plan_bindings(&db, &workflow_id).await;
    let runs_before = count_plan_runs(&db, &workflow_id).await;
    let delegation_runs_before = delegation_task_run::Entity::find()
        .count(&db.conn)
        .await
        .expect("count runs before recovery");

    let blocked_status = get_workflow_state_core(&db, parent, Some(&workflow_id))
        .await
        .expect("project blocked session-2566 status");
    assert_eq!(
        blocked_status.workflow_state,
        ManifestWorkflowState::Blocked
    );
    assert_eq!(blocked_status.manifest_revision, 8);
    let projected_recovery = blocked_status
        .recovery
        .as_ref()
        .expect("blocked status exposes recovery projection");
    assert_eq!(projected_recovery.disposition, "confirmation_required");
    assert_eq!(
        projected_recovery.proposed_action.as_deref(),
        Some("recover_workflow")
    );
    assert!(blocked_status
        .nodes
        .iter()
        .filter(|node| node.phase_id.as_deref() == Some(PHASE_TASKS))
        .all(|node| node.latest_task_id.is_none()));

    let decision = workflow_recovery_decision(&db, &workflow_id).await;
    assert_eq!(
        decision.confirmation,
        WorkflowRecoveryConfirmation::Required
    );
    assert_eq!(decision.proposed_action(), Some("recover_workflow"));

    let mut direct_publication = session_2566_document();
    direct_publication.workflow_id = Some(workflow_id.clone());
    direct_publication.expected_manifest_revision = Some(8);
    direct_publication.workflow_state = ManifestWorkflowState::Approved;
    direct_publication.publication_token = "session-2566-initial".into();
    let rejected = publish_workflow_manifest_core(
        &db,
        &emitter,
        parent,
        PublishWorkflowRequest {
            document: direct_publication,
        },
    )
    .await
    .expect("blocked publication returns recovery-required disposition");
    assert_eq!(
        rejected.disposition,
        WorkflowPublicationDisposition::WorkflowRecoveryRequired
    );
    assert_eq!(load_header(&db, &workflow_id).await, blocked_header);
    assert_eq!(retired_bindings(&db, &workflow_id).await, retired_before);

    let authorization_id = approve_workflow_recovery(&db, parent, &workflow_id, &decision).await;
    let recovered = recover_workflow_core(
        &db,
        &emitter,
        parent,
        RecoverWorkflowRequest {
            workflow_id: workflow_id.clone(),
            recovery_authorization_id: authorization_id.clone(),
            expected_manifest_revision: 8,
            correlation_id: "session-2566-recovery".into(),
        },
    )
    .await
    .expect("authorized in-place recovery");
    assert_eq!(recovered.manifest_revision, 9);
    assert_eq!(recovered.new_state, ManifestWorkflowState::Approved);
    let approved_header = load_header(&db, &workflow_id).await;
    assert_eq!(approved_header.active_manifest_revision, 9);
    assert_eq!(
        approved_header.workflow_state,
        crate::db::entities::delegation_workflow::WorkflowState::Approved
    );
    assert_eq!(
        approved_header.structural_revision,
        blocked_header.structural_revision
    );
    assert_eq!(
        approved_header.design_fingerprint,
        blocked_header.design_fingerprint
    );
    assert_eq!(
        approved_header.plan_fingerprint,
        blocked_header.plan_fingerprint
    );
    let approved_revision =
        delegation_workflow_manifest_revision::Entity::find_by_id((workflow_id.clone(), 9))
            .one(&db.conn)
            .await
            .expect("load recovery revision")
            .expect("recovery revision");
    assert_eq!(
        approved_revision.revision_kind.as_deref(),
        Some("state_only")
    );
    assert_eq!(retired_bindings(&db, &workflow_id).await, retired_before);
    assert_eq!(count_plan_runs(&db, &workflow_id).await, runs_before);
    assert_eq!(
        delegation_task_run::Entity::find()
            .count(&db.conn)
            .await
            .expect("count runs after recovery"),
        delegation_runs_before,
        "workflow recovery must not create Author or reviewer runs"
    );
    let receipt = RecoveryAuthorizationStore::new(db.conn.clone())
        .find_by_id(&authorization_id)
        .await
        .expect("read workflow receipt")
        .expect("workflow receipt");
    assert_eq!(
        receipt.status,
        recovery_authorization::RecoveryAuthorizationStatus::Consumed
    );

    let task_child = seed_conversation(
        &db,
        seed_folder(&db, "/tmp/session-2566-task-1").await,
        AgentType::Grok,
    )
    .await;
    insert_unbound_run(
        &db,
        parent,
        task_child,
        "session-2566-task-1",
        "session-2566-task-1",
        "grok",
        task_work_unit_key(),
        AdmissionClass::NormalRevision,
    )
    .await;
    let txn = db.conn.begin().await.expect("begin first dispatch");
    admit_workflow_run_txn(
        &txn,
        &WorkflowAdmitInput {
            parent_conversation_id: parent,
            child_conversation_id: task_child,
            task_id: "session-2566-task-1",
            work_unit_key: Some(&task_work_unit_key()),
            agent_type: "grok",
            profile_id: None,
            lineage_root_task_id: "session-2566-task-1",
            generation: 1,
            kind: AdmissionDispatchKind::FirstDispatch,
            admission_class: AdmissionClass::NormalRevision,
            workspace_path: None,
        },
    )
    .await
    .expect("Task 1 first dispatch must pass the exact Plan gate");
    txn.commit().await.expect("commit Task 1 admission");
}

#[tokio::test]
async fn legacy_parent_disconnect_authorize_continue_then_unresumable_replace() {
    let (db, runs, parent, child, source_task_id) = seed_legacy_parent_disconnect().await;
    let spawner = Arc::new(MockSpawner::new());
    let broker = broker_for_recovery(db.clone(), runs.clone(), spawner.clone()).await;

    let rejected = broker
        .continue_delegation(continue_request(&source_task_id, parent, None))
        .await;
    assert_eq!(
        rejected.error_code.as_deref(),
        Some("recovery_confirmation_required")
    );

    let source = runs
        .load_by_task_id(&source_task_id)
        .await
        .expect("load source")
        .expect("source");
    let eligibility = runs
        .build_continue_eligibility(&source)
        .await
        .expect("continue eligibility");
    let decision = decide_delegation_recovery(
        &crate::acp::delegation::run_store::recovery_source_from_continue_eligibility(&eligibility),
        &RecoveryRailSnapshot {
            agent_supports_reuse: eligibility.agent_supports_reuse,
            unexpected_continue_budget_available: eligibility.unexpected_continue_budget_available,
            replacement_budget_available: eligibility.replacement_budget_available,
        },
        RequestedRecoveryOperation::Continue,
    );
    assert_eq!(decision.confirmation, RecoveryConfirmation::Required);
    let authorization_id = approve_continue_recovery(
        &db,
        parent,
        child,
        &source_task_id,
        "legacy-unit",
        &decision,
    )
    .await;
    spawner
        .queue_spawn(Err(SpawnerError::Spawn("resume transport lost".into())))
        .await;
    let failed = broker
        .continue_delegation(continue_request(
            &source_task_id,
            parent,
            Some(authorization_id.clone()),
        ))
        .await;
    assert_eq!(failed.error_code.as_deref(), Some("unresumable"));
    let continued_task_id = failed.task_id.expect("continued task id");
    let continued = runs
        .load_by_task_id(&continued_task_id)
        .await
        .expect("load continued")
        .expect("continued run");
    assert_eq!(continued.run_status, DelegationRunStatus::Failed);
    assert_eq!(continued.error_code.as_deref(), Some("unresumable"));
    assert_eq!(
        continued.recovery_authorization_id.as_deref(),
        Some(authorization_id.as_str())
    );
    let receipt = RecoveryAuthorizationStore::new(db.conn.clone())
        .find_by_id(&authorization_id)
        .await
        .expect("receipt read")
        .expect("receipt");
    assert_eq!(
        receipt.status,
        recovery_authorization::RecoveryAuthorizationStatus::Consumed
    );
    assert_eq!(
        receipt.consumed_by_id.as_deref(),
        Some(continued_task_id.as_str())
    );

    let stale_replacement_child = conversation_service::create_with_delegation(
        &db.conn,
        seed_folder(&db, "/tmp/legacy-stale-replacement").await,
        AgentType::Codex,
        Some("stale replacement".into()),
        None,
        Some(crate::acp::delegation::spawner::DelegationLink {
            parent_conversation_id: parent,
            parent_tool_use_id: "legacy-stale-replacement-tool".into(),
            delegation_call_id: "legacy-stale-replacement".into(),
        }),
    )
    .await
    .expect("stale replacement child");
    let run_count_before_stale_replacement = delegation_task_run::Entity::find()
        .count(&db.conn)
        .await
        .expect("count runs before stale replacement");
    runs.admit_gen1_reserving(replacement_insert(
        parent,
        stale_replacement_child.id,
        &source_task_id,
        &source_task_id,
    ))
    .await
    .expect_err("replacement must start from the new latest unresumable run");
    assert_eq!(
        delegation_task_run::Entity::find()
            .count(&db.conn)
            .await
            .expect("count runs after stale replacement"),
        run_count_before_stale_replacement
    );

    let replacement_child = conversation_service::create_with_delegation(
        &db.conn,
        seed_folder(&db, "/tmp/legacy-replacement").await,
        AgentType::Codex,
        Some("replacement".into()),
        None,
        Some(crate::acp::delegation::spawner::DelegationLink {
            parent_conversation_id: parent,
            parent_tool_use_id: "legacy-replacement-tool".into(),
            delegation_call_id: "legacy-replacement".into(),
        }),
    )
    .await
    .expect("replacement child");
    let replacement = runs
        .admit_gen1_reserving(replacement_insert(
            parent,
            replacement_child.id,
            &source_task_id,
            &continued_task_id,
        ))
        .await
        .expect("replacement from new latest unresumable run");
    assert!(matches!(
        replacement,
        crate::acp::delegation::run_store::Gen1AdmitOutcome::Created(_)
    ));
    let inherited = runs
        .load_by_task_id("legacy-replacement")
        .await
        .expect("load replacement")
        .expect("replacement");
    assert_eq!(
        inherited.recovery_authorization_id.as_deref(),
        Some(authorization_id.as_str())
    );
    let receipt_after = RecoveryAuthorizationStore::new(db.conn.clone())
        .find_by_id(&authorization_id)
        .await
        .expect("receipt read after replacement")
        .expect("receipt after replacement");
    assert_eq!(receipt_after, receipt, "replacement must not consume again");
}

const SESSION_2566_WORKFLOW_ID: &str = "afd89cd7-5df0-49d9-8a40-1d2c95791cbd";
const SESSION_2566_PLAN_DIGEST: &str =
    "sha256:77fca1481d57395b3b7fe090be2d116e647f6275e303895b0b88e7ad4428d4b5";

async fn load_header(db: &crate::db::AppDatabase, workflow_id: &str) -> delegation_workflow::Model {
    delegation_workflow::Entity::find_by_id(workflow_id.to_string())
        .one(&db.conn)
        .await
        .expect("load workflow header")
        .expect("workflow header")
}

async fn rename_workflow_id(db: &crate::db::AppDatabase, initial: &str) -> String {
    // New workflows intentionally reject caller-supplied IDs. Re-key only this
    // in-memory fixture's fresh rows so the reconstructed production ID is
    // durable rather than merely asserted in test prose.
    db.conn
        .execute_unprepared("PRAGMA foreign_keys = OFF")
        .await
        .expect("disable fixture foreign keys");
    for table in [
        "delegation_workflow_node_bindings",
        "delegation_workflow_manifest_revisions",
        "delegation_workflows",
    ] {
        db.conn
            .execute_unprepared(&format!(
                "UPDATE {table} SET workflow_id = '{SESSION_2566_WORKFLOW_ID}' WHERE workflow_id = '{initial}'"
            ))
            .await
            .expect("re-key fresh workflow fixture");
    }
    db.conn
        .execute_unprepared("PRAGMA foreign_keys = ON")
        .await
        .expect("restore fixture foreign keys");
    SESSION_2566_WORKFLOW_ID.into()
}

#[allow(clippy::too_many_arguments)]
async fn insert_plan_run(
    db: &crate::db::AppDatabase,
    parent: i32,
    workflow_id: &str,
    node_id: &str,
    task_id: &str,
    agent_type: &str,
    reviewed_task_id: Option<&str>,
    observed: bool,
    summary: serde_json::Value,
) {
    let child = seed_conversation(
        db,
        seed_folder(db, &format!("/tmp/{task_id}")).await,
        AgentType::Codex,
    )
    .await;
    let binding = delegation_workflow_node_binding::Entity::find_by_id((
        workflow_id.to_string(),
        node_id.to_string(),
    ))
    .one(&db.conn)
    .await
    .expect("load Plan binding")
    .expect("Plan binding");
    insert_unbound_run(
        db,
        parent,
        child,
        task_id,
        task_id,
        agent_type,
        binding.work_unit_key.clone(),
        AdmissionClass::NormalRevision,
    )
    .await;
    let now = Utc::now();
    let mut run = delegation_task_run::Entity::find_by_id(task_id.to_string())
        .one(&db.conn)
        .await
        .expect("load Plan run")
        .expect("Plan run")
        .into_active_model();
    run.status = Set(DelegationRunStatus::Completed);
    run.finished_at = Set(Some(now));
    run.card_summary_json = Set(Some(summary.to_string()));
    run.update(&db.conn).await.expect("complete Plan run");
    delegation_workflow_run_binding::ActiveModel {
        task_id: Set(task_id.to_string()),
        workflow_id: Set(workflow_id.to_string()),
        node_id: Set(node_id.to_string()),
        gate_id: Set(Some("plan".into())),
        gate_cycle: Set(Some(1)),
        manifest_revision: Set(1),
        content_fingerprint: Set(Some(load_header(db, workflow_id).await.plan_fingerprint)),
        artifact_digest: Set(Some(SESSION_2566_PLAN_DIGEST.into())),
        reviewed_task_id: Set(reviewed_task_id.map(str::to_string)),
        reviewed_implementer_generation: Set(None),
        lineage_ordinal: Set(1),
        summary_validated: Set(true),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&db.conn)
    .await
    .expect("insert Plan run binding");
    if observed {
        let mut binding = binding.into_active_model();
        binding.is_observed = Set(true);
        binding
            .update(&db.conn)
            .await
            .expect("mark Plan node observed");
    }
}

#[allow(clippy::too_many_arguments)]
async fn insert_unbound_run(
    db: &crate::db::AppDatabase,
    parent: i32,
    child: i32,
    task_id: &str,
    root_task_id: &str,
    agent_type: &str,
    work_unit_key: String,
    admission_class: AdmissionClass,
) {
    let now = Utc::now();
    delegation_task_run::ActiveModel {
        task_id: Set(task_id.to_string()),
        root_task_id: Set(root_task_id.to_string()),
        previous_task_id: Set(None),
        generation: Set(1),
        parent_conversation_id: Set(parent),
        parent_tool_use_id: Set(Some(format!("tool-{task_id}"))),
        child_conversation_id: Set(child),
        agent_type: Set(agent_type.into()),
        profile_id: Set(None),
        workspace_path: Set(Some("/tmp/session-2566".into())),
        route_fingerprint: Set(Some("session-2566-route".into())),
        launch_snapshot_version: Set(Some("v1".into())),
        mode_id: Set(None),
        config_values_json: Set(Some("{}".into())),
        task_preview: Set(Some("session fixture".into())),
        request_fingerprint: Set(Some(format!("request-{task_id}"))),
        admission_class: Set(admission_class),
        reached_running_at: Set(Some(now)),
        lineage_root_task_id: Set(root_task_id.to_string()),
        work_unit_key: Set(Some(work_unit_key)),
        legacy_parent_tool_use_id: Set(None),
        history_only: Set(false),
        status: Set(DelegationRunStatus::Reserving),
        error_code: Set(None),
        termination_audit_json: Set(None),
        started_at: Set(Some(now)),
        finished_at: Set(None),
        tool_call_count: Set(None),
        edit_tool_call_count: Set(None),
        touched_files_json: Set(None),
        touched_files_truncated: Set(None),
        additions: Set(None),
        deletions: Set(None),
        line_counts_complete: Set(None),
        card_summary_json: Set(Some("{}".into())),
        child_turn_anchor: Set(None),
        child_connection_id: Set(None),
        replaced_task_id: Set(None),
        replacement_reason: Set(None),
        recovery_authorization_id: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&db.conn)
    .await
    .expect("insert fixture run");
}

async fn append_session_revisions(db: &crate::db::AppDatabase, workflow_id: &str) {
    for target in [
        ManifestWorkflowState::Approved,
        ManifestWorkflowState::Approved,
        ManifestWorkflowState::Approved,
        ManifestWorkflowState::Approved,
        ManifestWorkflowState::Approved,
        ManifestWorkflowState::Blocked,
    ] {
        let header = load_header(db, workflow_id).await;
        let txn = db.conn.begin().await.expect("begin state revision");
        append_state_only_revision_txn(
            &txn,
            &header,
            StateOnlyRevisionRequest {
                target_state: target,
                transition_reason_code: if target == ManifestWorkflowState::Blocked {
                    WorkflowBlockCause::ExplicitManifestBlock.as_str()
                } else {
                    "session_2566_state_reconstruction"
                },
                recovery_authorization_id: None,
                consumer_correlation_id: None,
            },
            Utc::now(),
        )
        .await
        .expect("append session revision");
        txn.commit().await.expect("commit session revision");
    }
}

async fn insert_retired_plan_bindings(
    db: &crate::db::AppDatabase,
    workflow_id: &str,
) -> Vec<delegation_workflow_node_binding::Model> {
    let now = Utc::now();
    for index in 1..=4 {
        delegation_workflow_node_binding::ActiveModel {
            workflow_id: Set(workflow_id.to_string()),
            node_id: Set(format!("retired-plan-{index}")),
            work_unit_key: Set(format!("retired:plan:{index}")),
            role: Set("reviewer".into()),
            agent_type: Set("codex".into()),
            profile_id: Set(None),
            phase_id: Set(PHASE_PLAN.into()),
            task_index: Set(None),
            introduced_revision: Set(1),
            retired_revision: Set(Some(8)),
            is_observed: Set(true),
            retained_observed: Set(true),
            cohort_frozen: Set(false),
            node_outcome: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&db.conn)
        .await
        .expect("insert retired Plan binding");
    }
    retired_bindings(db, workflow_id).await
}

async fn retired_bindings(
    db: &crate::db::AppDatabase,
    workflow_id: &str,
) -> Vec<delegation_workflow_node_binding::Model> {
    delegation_workflow_node_binding::Entity::find()
        .filter(delegation_workflow_node_binding::Column::WorkflowId.eq(workflow_id.to_string()))
        .filter(delegation_workflow_node_binding::Column::RetiredRevision.eq(8))
        .order_by_asc(delegation_workflow_node_binding::Column::NodeId)
        .all(&db.conn)
        .await
        .expect("load retired Plan bindings")
}

async fn count_plan_runs(db: &crate::db::AppDatabase, workflow_id: &str) -> u64 {
    delegation_workflow_run_binding::Entity::find()
        .filter(delegation_workflow_run_binding::Column::WorkflowId.eq(workflow_id.to_string()))
        .filter(delegation_workflow_run_binding::Column::GateId.eq("plan"))
        .count(&db.conn)
        .await
        .expect("count Plan runs")
}

async fn workflow_recovery_decision(
    db: &crate::db::AppDatabase,
    workflow_id: &str,
) -> WorkflowRecoveryDecision {
    let header = load_header(db, workflow_id).await;
    let txn = db.conn.begin().await.expect("begin recovery decision");
    let snapshot = load_workflow_recovery_snapshot_txn(&txn, &header, None)
        .await
        .expect("load recovery snapshot");
    txn.rollback().await.expect("rollback recovery decision");
    decide_workflow_recovery(&snapshot)
}

async fn approve_workflow_recovery(
    db: &crate::db::AppDatabase,
    parent: i32,
    workflow_id: &str,
    decision: &WorkflowRecoveryDecision,
) -> String {
    let service = RecoveryAuthorizationService::new(db.conn.clone());
    let prepared = service
        .prepare(RecoveryChallenge {
            parent_conversation_id: parent,
            subject_kind: RecoverySubjectKind::Workflow,
            subject_id: workflow_id.into(),
            delegation_identity: None,
            source_state_fingerprint: decision.source_state_fingerprint.clone(),
            allowed_action: RecoveryAllowedAction::RecoverWorkflow,
            action_payload: decision.action_payload().expect("workflow action payload"),
            cause_code: decision.cause_code.as_str().into(),
            risk_class: decision.risk_class.as_str().into(),
            display_reason: None,
        })
        .await
        .expect("prepare workflow authorization");
    let authorization_id = match prepared {
        PreparedAuthorization::Pending { row, .. } => row.authorization_id,
        other => panic!("expected pending workflow authorization, got {other:?}"),
    };
    service
        .resolve_question(&authorization_id, approve_outcome())
        .await
        .expect("approve workflow authorization");
    authorization_id
}

fn approve_outcome() -> QuestionOutcome {
    QuestionOutcome {
        answers: vec![QuestionAnsweredItem {
            question: "recovery_authorization".into(),
            header: "Recovery".into(),
            multi_select: false,
            selected: vec![RECOVERY_APPROVE_LABEL.into()],
        }],
        declined: false,
    }
}

fn task_work_unit_key() -> String {
    build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
        task_index: 1,
        agent_type: "grok",
        profile_id: None,
    })
    .expect("Task 1 key")
}

async fn seed_legacy_parent_disconnect(
) -> (Arc<crate::db::AppDatabase>, Arc<RunStore>, i32, i32, String) {
    let db = Arc::new(fresh_in_memory_db().await);
    let parent = conversation_service::create(
        &db.conn,
        seed_folder(&db, "/tmp/legacy-parent").await,
        AgentType::Codex,
        Some("legacy parent".into()),
        None,
    )
    .await
    .expect("legacy parent");
    let source_task_id = "legacy-source".to_string();
    let child = conversation_service::create_with_delegation(
        &db.conn,
        seed_folder(&db, "/tmp/legacy-child").await,
        AgentType::Codex,
        Some("legacy child".into()),
        None,
        Some(crate::acp::delegation::spawner::DelegationLink {
            parent_conversation_id: parent.id,
            parent_tool_use_id: "legacy-source-tool".into(),
            delegation_call_id: source_task_id.clone(),
        }),
    )
    .await
    .expect("legacy child");
    let store = Arc::new(RunStore::new(db.clone()));
    store
        .insert_reserving(ReservingRunInsert {
            task_id: source_task_id.clone(),
            root_task_id: source_task_id.clone(),
            previous_task_id: None,
            generation: 1,
            parent_conversation_id: parent.id,
            parent_tool_use_id: Some("legacy-source-tool".into()),
            child_conversation_id: child.id,
            agent_type: "codex".into(),
            profile_id: None,
            workspace_path: Some("/tmp/legacy".into()),
            route_fingerprint: Some("legacy-route".into()),
            launch_snapshot_version: Some("v1".into()),
            mode_id: None,
            config_values_json: Some("{}".into()),
            task_preview: Some("legacy task".into()),
            request_fingerprint: Some("legacy-request".into()),
            admission_class: AdmissionClass::NormalRevision,
            lineage_root_task_id: source_task_id.clone(),
            work_unit_key: Some("legacy-unit".into()),
            history_only: false,
            replaced_task_id: None,
            replacement_reason: None,
            started_at: Some(Utc::now()),
        })
        .await
        .expect("source reserve");
    store
        .bind_child_connection_while_reserving(&source_task_id, "legacy-child-connection")
        .await
        .expect("bind source");
    store
        .promote_running(&source_task_id, "legacy-child-connection", Utc::now())
        .await
        .expect("promote source");
    let source = delegation_task_run::Entity::find_by_id(&source_task_id)
        .one(&db.conn)
        .await
        .expect("load source")
        .expect("source");
    let mut source = source.into_active_model();
    source.status = Set(DelegationRunStatus::Canceled);
    source.error_code = Set(Some("parent_disconnected".into()));
    source.termination_audit_json = Set(None);
    source.finished_at = Set(Some(Utc::now()));
    source.update(&db.conn).await.expect("legacy disconnect");
    let child_row = crate::db::entities::conversation::Entity::find_by_id(child.id)
        .one(&db.conn)
        .await
        .expect("load legacy child")
        .expect("legacy child row");
    let mut child_row = child_row.into_active_model();
    child_row.external_id = Set(Some("legacy-external-session".into()));
    child_row
        .update(&db.conn)
        .await
        .expect("set external session");
    (db, store, parent.id, child.id, source_task_id)
}

async fn broker_for_recovery(
    db: Arc<crate::db::AppDatabase>,
    runs: Arc<RunStore>,
    spawner: Arc<MockSpawner>,
) -> Arc<DelegationBroker> {
    let task_store = Arc::new(DbDelegationTaskStore::from_run_store(runs.clone()))
        as Arc<dyn DelegationTaskStore>;
    let broker = Arc::new(
        DelegationBroker::new(
            spawner as Arc<dyn ConnectionSpawner>,
            Arc::new(DbDepthLookup { db }),
        )
        .with_task_store(task_store)
        .with_run_store(runs),
    );
    broker
        .set_config(DelegationConfig {
            enabled: true,
            ..DelegationConfig::default()
        })
        .await;
    broker
}

fn continue_request(
    source_task_id: &str,
    parent: i32,
    authorization_id: Option<String>,
) -> ContinueDelegationRequest {
    ContinueDelegationRequest {
        parent_connection_id: "legacy-parent-connection".into(),
        parent_conversation_id: parent,
        parent_tool_use_id: "legacy-continue-tool".into(),
        target_task_id: source_task_id.into(),
        task: "continue legacy task".into(),
        work_unit_key: Some("legacy-unit".into()),
        external_handle: None,
        correlation_id: Some("legacy-continue-correlation".into()),
        recovery_authorization_id: authorization_id,
    }
}

async fn approve_continue_recovery(
    db: &crate::db::AppDatabase,
    parent: i32,
    child: i32,
    source_task_id: &str,
    work_unit_key: &str,
    decision: &RecoveryDecision,
) -> String {
    let store = RecoveryAuthorizationStore::new(db.conn.clone());
    let now = Utc::now();
    let projection = DelegationRecoveryProjection::from(decision);
    let pending = store
        .insert_pending(
            &RecoveryChallenge {
                parent_conversation_id: parent,
                subject_kind: RecoverySubjectKind::DelegationTask,
                subject_id: source_task_id.into(),
                delegation_identity: Some(DelegationAuthorizationIdentity {
                    source_task_id: source_task_id.into(),
                    child_conversation_id: Some(child),
                    lineage_root_task_id: source_task_id.into(),
                    work_unit_key: Some(work_unit_key.into()),
                }),
                source_state_fingerprint: decision.source_state_fingerprint.clone(),
                allowed_action: RecoveryAllowedAction::Continue,
                action_payload: crate::acp::delegation::run_store::recovery_action_payload(
                    &RequestedRecoveryOperation::Continue,
                ),
                cause_code: projection.cause_code,
                risk_class: projection.risk_class,
                display_reason: None,
            },
            now,
        )
        .await
        .expect("create fixed authorization");
    store
        .approve_pending(&pending.authorization_id, now, now + Duration::minutes(10))
        .await
        .expect("approve continue authorization");
    pending.authorization_id
}

fn replacement_insert(
    parent: i32,
    child: i32,
    root_task_id: &str,
    continued_task_id: &str,
) -> ReservingRunInsert {
    ReservingRunInsert {
        task_id: "legacy-replacement".into(),
        root_task_id: "legacy-replacement".into(),
        previous_task_id: Some(continued_task_id.into()),
        generation: 1,
        parent_conversation_id: parent,
        parent_tool_use_id: Some("legacy-replacement-tool".into()),
        child_conversation_id: child,
        agent_type: "codex".into(),
        profile_id: None,
        workspace_path: Some("/tmp/legacy".into()),
        route_fingerprint: Some("legacy-route".into()),
        launch_snapshot_version: Some("v1".into()),
        mode_id: None,
        config_values_json: Some("{}".into()),
        task_preview: Some("replacement".into()),
        request_fingerprint: Some("legacy-replacement-request".into()),
        admission_class: AdmissionClass::Replacement,
        lineage_root_task_id: root_task_id.into(),
        work_unit_key: Some("legacy-unit".into()),
        history_only: false,
        replaced_task_id: Some(continued_task_id.into()),
        replacement_reason: Some("unresumable".into()),
        started_at: Some(Utc::now()),
    }
}

fn session_2566_document() -> ManifestDocument {
    let mut document = ManifestDocument {
        schema_version: MANIFEST_SCHEMA_VERSION,
        workflow_kind: WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY.into(),
        plan_target_rel_path: "docs/superpowers/plans/session-2566.md".into(),
        risk_policy_version: "b2d_task_risk_v1".into(),
        workflow_id: None,
        expected_manifest_revision: None,
        publication_token: "session-2566-initial".into(),
        workflow_state: ManifestWorkflowState::Estimated,
        design: Some(DocumentRef {
            rel_path: "docs/superpowers/specs/session-2566.md".into(),
            digest: "sha256:session-2566-design".into(),
        }),
        plan: Some(DocumentRef {
            rel_path: "docs/superpowers/plans/session-2566.md".into(),
            digest: SESSION_2566_PLAN_DIGEST.into(),
        }),
        phases: vec![
            phase(PHASE_DESIGN),
            phase(PHASE_PLAN),
            phase(PHASE_TASKS),
            phase(PHASE_FINAL),
        ],
        nodes: vec![],
        edges: vec![],
        gates: vec![],
        task_policies: vec![],
    };
    document.nodes = vec![
        node(
            "design-reviewer-1",
            PHASE_DESIGN,
            ManifestNodeRole::Reviewer,
            "codex",
            None,
            vec![],
        ),
        node(
            "plan-author",
            PHASE_PLAN,
            ManifestNodeRole::Author,
            "codex",
            None,
            vec![],
        ),
        node(
            "plan-reviewer-1",
            PHASE_PLAN,
            ManifestNodeRole::Reviewer,
            "codex",
            None,
            vec!["plan-author"],
        ),
        node(
            "plan-reviewer-2",
            PHASE_PLAN,
            ManifestNodeRole::Reviewer,
            "grok",
            None,
            vec!["plan-author"],
        ),
        node(
            "task-1-impl",
            PHASE_TASKS,
            ManifestNodeRole::Implementer,
            "grok",
            Some(1),
            vec!["plan-reviewer-1", "plan-reviewer-2"],
        ),
        node(
            "task-1-rev",
            PHASE_TASKS,
            ManifestNodeRole::Reviewer,
            "codex",
            Some(1),
            vec!["task-1-impl"],
        ),
        node(
            "final-reviewer",
            PHASE_FINAL,
            ManifestNodeRole::Reviewer,
            "codex",
            None,
            vec!["task-1-rev"],
        ),
        node(
            "final-fixer",
            PHASE_FINAL,
            ManifestNodeRole::Fixer,
            "grok",
            None,
            vec!["final-reviewer"],
        ),
    ];
    document.edges = vec![ManifestEdge {
        id: Some("task-1-review".into()),
        from: "task-1-impl".into(),
        to: "task-1-rev".into(),
    }];
    document.gates = vec![
        ManifestGate {
            id: "design".into(),
            reviewer_cohort_node_ids: vec!["design-reviewer-1".into()],
            required_reviewer_node_ids: vec!["design-reviewer-1".into()],
            resolution_mode: ResolutionMode::ParentAdjudication,
            gate_kind: Some(DocumentGateKind::Design),
        },
        ManifestGate {
            id: "plan".into(),
            reviewer_cohort_node_ids: vec!["plan-reviewer-1".into(), "plan-reviewer-2".into()],
            required_reviewer_node_ids: vec!["plan-reviewer-1".into(), "plan-reviewer-2".into()],
            resolution_mode: ResolutionMode::ParentAdjudication,
            gate_kind: Some(DocumentGateKind::Plan),
        },
    ];
    document.task_policies = vec![ManifestTaskPolicy {
        task_index: 1,
        risk: ManifestTaskRisk {
            level: TaskRiskLevel::Normal,
            hard_triggers: vec![],
            soft_signals: vec![],
            score: 0,
            reason: "normal session-2566 task".into(),
        },
        route: ManifestTaskRoute {
            implementer_node_id: "task-1-impl".into(),
            reviewer_node_ids: vec!["task-1-rev".into()],
        },
    }];
    document
}

fn phase(id: &str) -> ManifestPhase {
    ManifestPhase {
        id: id.into(),
        kind: Some(id.into()),
        title: None,
    }
}

fn node(
    id: &str,
    phase_id: &str,
    role: ManifestNodeRole,
    agent_type: &str,
    task_index: Option<u32>,
    deps: Vec<&str>,
) -> ManifestNode {
    let work_unit_key = match role {
        ManifestNodeRole::Author => build_work_unit_key(&WorkUnitKeyParts::PlanAuthor {
            rel_plan_path: "docs/superpowers/plans/session-2566.md",
            agent_type,
            profile_id: None,
        }),
        ManifestNodeRole::Reviewer if phase_id == PHASE_PLAN => {
            build_work_unit_key(&WorkUnitKeyParts::PlanReviewer {
                rel_plan_path: "docs/superpowers/plans/session-2566.md",
                agent_type,
                profile_id: None,
            })
        }
        ManifestNodeRole::Implementer => build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: task_index.expect("task index"),
            agent_type,
            profile_id: None,
        }),
        ManifestNodeRole::Reviewer if phase_id == PHASE_TASKS => {
            build_work_unit_key(&WorkUnitKeyParts::TaskReviewer {
                task_index: task_index.expect("task index"),
                agent_type,
                profile_id: None,
            })
        }
        ManifestNodeRole::Reviewer if phase_id == PHASE_DESIGN => {
            build_work_unit_key(&WorkUnitKeyParts::Design {
                rel_doc_path: "docs/superpowers/specs/session-2566.md",
                agent_type,
                profile_id: None,
            })
        }
        ManifestNodeRole::Reviewer if phase_id == PHASE_FINAL => {
            build_work_unit_key(&WorkUnitKeyParts::FinalReviewer {
                agent_type,
                profile_id: None,
            })
        }
        ManifestNodeRole::Fixer => build_work_unit_key(&WorkUnitKeyParts::FinalFixer {
            agent_type,
            profile_id: None,
        }),
        _ => unreachable!("fixture role"),
    }
    .expect("recognized work unit key");
    ManifestNode {
        id: id.into(),
        kind: ManifestNodeKind::WorkUnit,
        phase_id: Some(phase_id.into()),
        role: Some(role),
        agent_type: Some(agent_type.into()),
        profile_id: None,
        task_index,
        work_unit_key: Some(work_unit_key),
        deps: deps.into_iter().map(str::to_string).collect(),
        required: Some(true),
        node_outcome: None,
        title: None,
    }
}
