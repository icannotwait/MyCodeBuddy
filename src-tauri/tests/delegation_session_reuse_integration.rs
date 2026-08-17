//! Task 9 — session-reuse integration / contract fixtures.
//!
//! Covers design conversation shapes 800 / 832 / 835, concurrent continue,
//! ResumeExistingOnly, preview/summary contracts, budget rails, pre-admission
//! redispatches, skill-forward routing invariants, and dual-surface snapshot
//! DTO identity. Uses in-memory DB + MockSpawner (no live multi-agent CLI).

#![cfg(feature = "test-utils")]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use codeg_lib::acp::delegation::broker::{
    ConversationDepthLookup, DelegationBroker, DelegationConfig, StatusWait,
};
use codeg_lib::acp::delegation::card_summary::{extract_card_summary, CARD_SUMMARY_MARKER};
use codeg_lib::acp::delegation::run_store::{
    derive_task_preview, request_fingerprint, ReservingRunInsert, RunStore, REPLACEMENT_LIMIT,
    REPLACEMENT_REASON_BUDGET_EXHAUSTED_CONTINUE, REPLACEMENT_REASON_UNRESUMABLE,
    UNEXPECTED_CONTINUE_LIMIT,
};
use codeg_lib::acp::delegation::spawner::{
    accepted, mock::MockSpawner, ConnectionSpawner, DelegationLink,
};
use codeg_lib::acp::delegation::store::{DbDelegationTaskStore, DelegationTaskStore};
use codeg_lib::acp::delegation::types::{
    ContinueDelegationRequest, DelegationError, DelegationOutcome, DelegationRequest,
    DelegationSuccess, TaskStatus, CONTINUE_DELEGATION_TOOL, DELEGATE_TO_AGENT_TOOL,
};
use codeg_lib::acp::termination::{
    AcpTerminationClassification, AcpTerminationReason, AcpTerminationSource,
    AcpTerminationSummaryV1, DelegationTerminationAuditV1,
};
use codeg_lib::app_state::AppState;
use codeg_lib::commands::delegation::get_delegation_run_snapshot_core;
use codeg_lib::db::entities::delegation_task_run::{
    AdmissionClass, DelegationRunStatus, Entity as DelegationTaskRun,
};
use codeg_lib::db::entities::{conversation, delegation_lineage_budget};
use codeg_lib::db::service::conversation_service;
use codeg_lib::db::test_helpers::{fresh_in_memory_db, seed_folder};
use codeg_lib::db::AppDatabase;
use codeg_lib::models::AgentType;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, Set};
use tokio::sync::Barrier;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct RootDepth;
#[async_trait]
impl ConversationDepthLookup for RootDepth {
    async fn parent_of(&self, _id: i32) -> Result<Option<i32>, DelegationError> {
        Ok(None)
    }
}

async fn broker_with_run_store(
    mock: Arc<MockSpawner>,
    parent_id: i32,
    run_store: Arc<RunStore>,
) -> Arc<DelegationBroker> {
    let depth = Arc::new(RootDepth) as Arc<dyn ConversationDepthLookup>;
    let task_store = Arc::new(DbDelegationTaskStore::new(run_store.db().clone()))
        as Arc<dyn DelegationTaskStore>;
    let broker = Arc::new(
        DelegationBroker::new(mock as Arc<dyn ConnectionSpawner>, depth)
            .with_task_store(task_store)
            .with_run_store(run_store),
    );
    broker
        .set_config(DelegationConfig {
            enabled: true,
            ..DelegationConfig::default()
        })
        .await;
    let _ = parent_id;
    broker
}

/// Session-reuse scenarios exercise lineage and persistence, not workspace
/// validation. Use the real test process directory so their legacy `/tmp/...`
/// labels remain portable on Windows as well as Unix hosts.
fn test_working_dir() -> String {
    std::fs::canonicalize(
        std::env::current_dir().expect("resolve integration test working directory"),
    )
    .expect("canonicalize integration test working directory")
    .to_string_lossy()
    .into_owned()
}

fn delegate_req(
    parent_id: i32,
    tool_use: &str,
    agent: AgentType,
    task: &str,
    _workdir: &str,
    work_unit: Option<&str>,
) -> DelegationRequest {
    DelegationRequest {
        parent_connection_id: "parent-conn".into(),
        parent_conversation_id: parent_id,
        parent_tool_use_id: tool_use.into(),
        agent_type: agent,
        profile_id: None,
        task: task.into(),
        working_dir: Some(test_working_dir()),
        requested_working_dir: None,
        external_handle: None,
        work_unit_key: work_unit.map(str::to_string),
        replaces_task_id: None,
        replacement_reason: None,
        // Explicit parent_tool_use_id fixtures do not need correlation_id.
        correlation_id: None,
        recovery_authorization_id: None,
        orchestration_binding: None,
    }
}

fn continue_req(
    parent_id: i32,
    tool_use: &str,
    target_task_id: &str,
    task: &str,
    work_unit: Option<&str>,
) -> ContinueDelegationRequest {
    ContinueDelegationRequest {
        parent_connection_id: "parent-conn".into(),
        parent_conversation_id: parent_id,
        parent_tool_use_id: tool_use.into(),
        target_task_id: target_task_id.into(),
        task: task.into(),
        work_unit_key: work_unit.map(str::to_string),
        external_handle: None,
        // Explicit parent_tool_use_id fixtures do not need correlation_id.
        correlation_id: None,
        recovery_authorization_id: None,
        orchestration_binding: None,
    }
}

fn typed_running_termination_audit(
    source: AcpTerminationSource,
    reason: AcpTerminationReason,
    admission_class: AdmissionClass,
) -> String {
    serde_json::to_string(&DelegationTerminationAuditV1::new(
        AcpTerminationSummaryV1::new(
            source,
            reason,
            AcpTerminationClassification::Unexpected,
            true,
            Utc::now(),
        ),
        DelegationRunStatus::Running,
        admission_class,
        None,
        None,
    ))
    .expect("serialize typed termination audit")
}

fn unexpected_host_restart_audit(admission_class: AdmissionClass) -> String {
    typed_running_termination_audit(
        AcpTerminationSource::HostRestart,
        AcpTerminationReason::HostRestarted,
        admission_class,
    )
}

fn unexpected_session_loss_audit(admission_class: AdmissionClass) -> String {
    typed_running_termination_audit(
        AcpTerminationSource::Session,
        AcpTerminationReason::SessionLost,
        admission_class,
    )
}

fn completed_outcome(text: &str, child_id: i32, agent: AgentType) -> DelegationOutcome {
    DelegationOutcome::Ok(DelegationSuccess {
        text: text.into(),
        child_conversation_id: child_id,
        child_agent_type: agent,
        turn_count: 1,
        duration_ms: 5,
        token_usage: None,
    })
}

async fn set_child_external_id(db: &AppDatabase, child_id: i32, external_id: &str) {
    let child = conversation::Entity::find_by_id(child_id)
        .one(&db.conn)
        .await
        .expect("child lookup")
        .expect("child row");
    let mut child = child.into_active_model();
    child.external_id = Set(Some(external_id.into()));
    child.update(&db.conn).await.expect("set external_id");
}

async fn start_and_complete(
    broker: &DelegationBroker,
    mock: &MockSpawner,
    req: DelegationRequest,
    conn_tag: &str,
) -> (String, i32) {
    mock.queue_spawn(Ok(format!("{conn_tag}-spawn"))).await;
    mock.queue_send(Ok(accepted(0, Utc::now()))).await;
    let ack = broker.start_delegation(req).await;
    assert_eq!(ack.status, TaskStatus::Running, "{ack:?}");
    let task_id = ack.task_id.expect("task_id");
    let child_id = ack.child_conversation_id.expect("child");
    let agent = ack.agent_type.unwrap_or(AgentType::Codex);
    broker
        .complete_call(&task_id, completed_outcome("ok", child_id, agent))
        .await;
    (task_id, child_id)
}

async fn continue_and_complete(
    broker: &DelegationBroker,
    mock: &MockSpawner,
    req: ContinueDelegationRequest,
    conn_tag: &str,
    child_id: i32,
    agent: AgentType,
) -> String {
    mock.queue_spawn(Ok(format!("{conn_tag}-resume"))).await;
    mock.queue_send(Ok(accepted(child_id, Utc::now()))).await;
    let ack = broker.continue_delegation(req).await;
    assert_eq!(ack.status, TaskStatus::Running, "{ack:?}");
    assert_eq!(ack.reused_session, Some(true), "{ack:?}");
    assert_eq!(ack.child_conversation_id, Some(child_id));
    let task_id = ack.task_id.expect("continued task_id");
    broker
        .complete_call(&task_id, completed_outcome("continue ok", child_id, agent))
        .await;
    task_id
}

async fn list_run_rows(
    db: &AppDatabase,
    parent_id: i32,
) -> Vec<codeg_lib::db::entities::delegation_task_run::Model> {
    DelegationTaskRun::find()
        .filter(
            codeg_lib::db::entities::delegation_task_run::Column::ParentConversationId
                .eq(parent_id),
        )
        .all(&db.conn)
        .await
        .expect("list runs")
}

fn unexpected_continue_insert(
    task_id: &str,
    parent_id: i32,
    child_id: i32,
    generation: i64,
    previous_task_id: Option<&str>,
    lineage_root_task_id: &str,
    work_unit_key: &str,
) -> ReservingRunInsert {
    ReservingRunInsert {
        orchestration_binding: None,
        task_id: task_id.into(),
        root_task_id: lineage_root_task_id.into(),
        previous_task_id: previous_task_id.map(str::to_string),
        generation,
        parent_conversation_id: parent_id,
        parent_tool_use_id: Some(format!("tu-{task_id}")),
        child_conversation_id: child_id,
        agent_type: "codex".into(),
        profile_id: None,
        workspace_path: Some(test_working_dir()),
        route_fingerprint: Some("aabbccdd".into()),
        launch_snapshot_version: Some("v1".into()),
        mode_id: Some("default".into()),
        config_values_json: Some("{}".into()),
        task_preview: Some(derive_task_preview("unexpected continuation")),
        request_fingerprint: Some(request_fingerprint(
            CONTINUE_DELEGATION_TOOL,
            "unexpected continuation",
            Some(work_unit_key),
            None,
            None,
            previous_task_id,
            "aabbccdd",
            None,
        )),
        admission_class: AdmissionClass::UnexpectedContinue,
        lineage_root_task_id: lineage_root_task_id.into(),
        work_unit_key: Some(work_unit_key.into()),
        history_only: false,
        replaced_task_id: None,
        replacement_reason: None,
        started_at: Some(Utc::now()),
    }
}

// ---------------------------------------------------------------------------
// 1. Conversation 800 shape — 3 children × 4 rounds = 12 runs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn shape_800_three_reviewers_four_rounds_twelve_runs() {
    let db = Arc::new(fresh_in_memory_db().await);
    let folder = seed_folder(&db, "/tmp/codeg-shape-800").await;
    let parent = conversation_service::create(
        &db.conn,
        folder,
        AgentType::ClaudeCode,
        Some("conv-800 parent".into()),
        None,
    )
    .await
    .expect("parent");
    let runs = Arc::new(RunStore::new(db.clone()));
    let mock = Arc::new(MockSpawner::new());
    let broker = broker_with_run_store(mock.clone(), parent.id, runs.clone()).await;
    let workdir = "/tmp/codeg-shape-800";

    // Three independent reviewers (design/plan/task-style units).
    let units = [
        ("reviewer-a", "design|doc|reviewer|none"),
        ("reviewer-b", "plan|plan|reviewer|none"),
        ("reviewer-c", "task|1|reviewer|none"),
    ];

    let mut latest: HashMap<&str, String> = HashMap::new();
    let mut children = BTreeSet::new();

    for (name, unit) in units {
        let (task_id, child_id) = start_and_complete(
            &broker,
            &mock,
            delegate_req(
                parent.id,
                &format!("tu-800-{name}-r1"),
                AgentType::Codex,
                &format!("round 1 for {name}"),
                workdir,
                Some(unit),
            ),
            name,
        )
        .await;
        set_child_external_id(&db, child_id, &format!("sess-{name}")).await;
        children.insert(child_id);
        latest.insert(name, task_id);
    }

    // Three more rounds continue each reviewer on the same child.
    for round in 2..=4 {
        for (name, unit) in units {
            let target = latest.get(name).expect("latest").clone();
            let child_id = {
                let run = runs.load_by_task_id(&target).await.unwrap().expect("run");
                run.child_conversation_id
            };
            let next = continue_and_complete(
                &broker,
                &mock,
                continue_req(
                    parent.id,
                    &format!("tu-800-{name}-r{round}"),
                    &target,
                    &format!("round {round} for {name}"),
                    Some(unit),
                ),
                &format!("{name}-r{round}"),
                child_id,
                AgentType::Codex,
            )
            .await;
            latest.insert(name, next);
        }
    }

    let rows = list_run_rows(&db, parent.id).await;
    assert_eq!(rows.len(), 12, "four rounds × three reviewers");
    assert_eq!(children.len(), 3, "exactly three child conversations");

    let mut by_child: HashMap<i32, usize> = HashMap::new();
    for row in &rows {
        *by_child.entry(row.child_conversation_id).or_default() += 1;
        assert_eq!(row.status, DelegationRunStatus::Completed);
    }
    for (child, count) in by_child {
        assert_eq!(count, 4, "child {child} must have four durable runs");
    }

    let visible = conversation_service::list_children(&db.conn, parent.id)
        .await
        .expect("children");
    assert_eq!(visible.len(), 3);
}

// ---------------------------------------------------------------------------
// 2. Conversation 832 — unexpected interrupt recovery, same child
// ---------------------------------------------------------------------------

#[tokio::test]
async fn shape_832_unexpected_interrupt_new_run_same_child() {
    let db = Arc::new(fresh_in_memory_db().await);
    let folder = seed_folder(&db, "/tmp/codeg-shape-832").await;
    let parent = conversation_service::create(
        &db.conn,
        folder,
        AgentType::ClaudeCode,
        Some("conv-832 parent".into()),
        None,
    )
    .await
    .expect("parent");
    let runs = Arc::new(RunStore::new(db.clone()));
    let mock = Arc::new(MockSpawner::new());
    let broker = broker_with_run_store(mock.clone(), parent.id, runs.clone()).await;

    let (root_task_id, child_id) = start_and_complete(
        &broker,
        &mock,
        delegate_req(
            parent.id,
            "tu-832-root",
            AgentType::Codex,
            "final review pass",
            "/tmp/codeg-shape-832",
            Some("final_review|main|reviewer|none"),
        ),
        "832-root",
    )
    .await;
    set_child_external_id(&db, child_id, "sess-832-codex").await;

    // Fixture: terminal canceled with structured unexpected interrupt audit
    // (conversation 832: interrupted before TurnComplete; session still present).
    let root = DelegationTaskRun::find_by_id(&root_task_id)
        .one(&db.conn)
        .await
        .unwrap()
        .expect("root");
    let mut root = root.into_active_model();
    root.status = Set(DelegationRunStatus::Canceled);
    root.error_code = Set(Some("host_restarted".into()));
    root.termination_audit_json = Set(Some(unexpected_host_restart_audit(
        root.admission_class.clone().unwrap(),
    )));
    root.update(&db.conn).await.expect("mark interrupted");

    mock.queue_spawn(Ok("832-recover".into())).await;
    mock.queue_send(Ok(accepted(child_id, Utc::now()))).await;
    let recovery = broker
        .continue_delegation(continue_req(
            parent.id,
            "tu-832-recover",
            &root_task_id,
            "resume after unexpected interrupt — new turn",
            Some("final_review|main|reviewer|none"),
        ))
        .await;
    assert_eq!(recovery.status, TaskStatus::Running, "{recovery:?}");
    assert_eq!(recovery.reused_session, Some(true));
    assert_eq!(recovery.child_conversation_id, Some(child_id));
    let recovery_id = recovery.task_id.expect("recovery task");

    let recovered = runs
        .load_by_task_id(&recovery_id)
        .await
        .unwrap()
        .expect("recovery run");
    assert_eq!(recovered.child_conversation_id, child_id);
    assert_eq!(
        recovered.admission_class,
        AdmissionClass::UnexpectedContinue
    );
    assert_eq!(
        recovered.previous_task_id.as_deref(),
        Some(root_task_id.as_str())
    );
    assert_ne!(
        recovered.task_id, root_task_id,
        "must not mutate canceled run"
    );

    let original = runs
        .load_by_task_id(&root_task_id)
        .await
        .unwrap()
        .expect("original still present");
    assert_eq!(original.run_status, DelegationRunStatus::Canceled);

    let children = conversation_service::list_children(&db.conn, parent.id)
        .await
        .expect("children");
    assert_eq!(children.len(), 1, "recovery stays on the same child");
}

// ---------------------------------------------------------------------------
// 3. Conversation 835 — replacement different child; original not_continuable
// ---------------------------------------------------------------------------

#[tokio::test]
async fn shape_835_replacement_supersedes_original_child() {
    let db = Arc::new(fresh_in_memory_db().await);
    let folder = seed_folder(&db, "/tmp/codeg-shape-835").await;
    let parent = conversation_service::create(
        &db.conn,
        folder,
        AgentType::ClaudeCode,
        Some("conv-835 parent".into()),
        None,
    )
    .await
    .expect("parent");
    let runs = Arc::new(RunStore::new(db.clone()));
    let mock = Arc::new(MockSpawner::new());
    let broker = broker_with_run_store(mock.clone(), parent.id, runs.clone()).await;
    let unit = "task|9|implementer|none";

    let (root_task_id, root_child) = start_and_complete(
        &broker,
        &mock,
        delegate_req(
            parent.id,
            "tu-835-root",
            AgentType::Grok,
            "implement task 9",
            "/tmp/codeg-shape-835",
            Some(unit),
        ),
        "835-root",
    )
    .await;

    // Source becomes unresumable (session lost / corrupt).
    broker
        .complete_call(
            &root_task_id,
            DelegationOutcome::from_err(
                DelegationError::Unresumable("session missing".into()),
                Some(root_child),
            ),
        )
        .await;
    // complete_call after already-completed is a no-op for terminal; force status.
    {
        let row = DelegationTaskRun::find_by_id(&root_task_id)
            .one(&db.conn)
            .await
            .unwrap()
            .expect("root");
        let mut row = row.into_active_model();
        row.status = Set(DelegationRunStatus::Failed);
        row.error_code = Set(Some("unresumable".into()));
        row.update(&db.conn).await.unwrap();
    }

    mock.queue_spawn(Ok("835-replacement".into())).await;
    mock.queue_send(Ok(accepted(0, Utc::now()))).await;
    let mut replacement = delegate_req(
        parent.id,
        "tu-835-replacement",
        AgentType::Grok,
        "replacement same role after unresumable",
        "/tmp/codeg-shape-835",
        Some(unit),
    );
    replacement.replaces_task_id = Some(root_task_id.clone());
    replacement.replacement_reason = Some(REPLACEMENT_REASON_UNRESUMABLE.into());
    let rep = broker.start_delegation(replacement).await;
    assert_eq!(rep.status, TaskStatus::Running, "{rep:?}");
    let rep_task_id = rep.task_id.expect("replacement task");
    let rep_child = rep.child_conversation_id.expect("replacement child");
    assert_ne!(
        rep_child, root_child,
        "replacement must use a different child"
    );

    let rep_run = runs
        .load_by_task_id(&rep_task_id)
        .await
        .unwrap()
        .expect("rep run");
    let rep_row = DelegationTaskRun::find_by_id(&rep_task_id)
        .one(&db.conn)
        .await
        .unwrap()
        .expect("replacement row");
    assert_eq!(
        rep_row.replaced_task_id.as_deref(),
        Some(root_task_id.as_str())
    );
    assert_eq!(
        rep_row.replacement_reason.as_deref(),
        Some(REPLACEMENT_REASON_UNRESUMABLE)
    );
    assert_eq!(rep_run.lineage_root_task_id, root_task_id);

    // Complete replacement so continue path can be exercised on it.
    set_child_external_id(&db, rep_child, "sess-835-rep").await;
    broker
        .complete_call(
            &rep_task_id,
            completed_outcome("replacement done", rep_child, AgentType::Grok),
        )
        .await;

    // Original child is not continuable (superseded by replaced_task_id pointer).
    let cont_orig = broker
        .continue_delegation(continue_req(
            parent.id,
            "tu-835-cont-orig",
            &root_task_id,
            "must not continue original",
            Some(unit),
        ))
        .await;
    assert_eq!(
        cont_orig.error_code.as_deref(),
        Some("not_continuable"),
        "{cont_orig:?}"
    );

    // Replacement child remains continuable.
    mock.queue_spawn(Ok("835-cont-rep".into())).await;
    mock.queue_send(Ok(accepted(rep_child, Utc::now()))).await;
    let cont_rep = broker
        .continue_delegation(continue_req(
            parent.id,
            "tu-835-cont-rep",
            &rep_task_id,
            "continue on replacement",
            Some(unit),
        ))
        .await;
    assert_eq!(cont_rep.status, TaskStatus::Running, "{cont_rep:?}");
    assert_eq!(cont_rep.child_conversation_id, Some(rep_child));
    assert_eq!(cont_rep.reused_session, Some(true));
}

// ---------------------------------------------------------------------------
// 4. Skill-forward routing invariants (approved v2 scenarios)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkillAction {
    Continue,
    FreshDelegate,
    Replacement,
    BlockNoSubstitute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SkillRoute {
    work_unit_key: &'static str,
    agent: AgentType,
}

#[derive(Debug)]
struct SkillScenario {
    name: &'static str,
    routes: &'static [SkillRoute],
    expected_actions: &'static [SkillAction],
    /// Each route must not reuse any of these prior work-unit keys.
    must_differ_from: &'static [&'static str],
    max_unexpected_continues: i64,
    max_replacements: i64,
}

fn skill_forward_v2_scenarios() -> Vec<SkillScenario> {
    vec![
        SkillScenario {
            name: "default_normal_grok_plus_codex_primary",
            routes: &[
                SkillRoute {
                    work_unit_key: "task|1|implementer|grok|none",
                    agent: AgentType::Grok,
                },
                SkillRoute {
                    work_unit_key: "task|1|reviewer|primary|codex|none",
                    agent: AgentType::Codex,
                },
            ],
            expected_actions: &[SkillAction::FreshDelegate],
            must_differ_from: &[],
            max_unexpected_continues: 2,
            max_replacements: 1,
        },
        SkillScenario {
            name: "selected_non_grok_normal_route",
            routes: &[
                SkillRoute {
                    work_unit_key: "task|2|implementer|gemini|careful",
                    agent: AgentType::Gemini,
                },
                SkillRoute {
                    work_unit_key: "task|2|reviewer|primary|codex|none",
                    agent: AgentType::Codex,
                },
            ],
            expected_actions: &[SkillAction::FreshDelegate],
            must_differ_from: &[],
            max_unexpected_continues: 2,
            max_replacements: 1,
        },
        SkillScenario {
            name: "high_codex_plus_primary_and_task_agent_auxiliary",
            routes: &[
                SkillRoute {
                    work_unit_key: "task|3|implementer|codex|none",
                    agent: AgentType::Codex,
                },
                SkillRoute {
                    work_unit_key: "task|3|reviewer|primary|codex|none",
                    agent: AgentType::Codex,
                },
                SkillRoute {
                    work_unit_key: "task|3|reviewer|auxiliary|grok|none",
                    agent: AgentType::Grok,
                },
            ],
            expected_actions: &[SkillAction::FreshDelegate],
            must_differ_from: &[],
            max_unexpected_continues: 2,
            max_replacements: 1,
        },
        SkillScenario {
            name: "codex_task_agent_keeps_three_distinct_units",
            routes: &[
                SkillRoute {
                    work_unit_key: "task|4|implementer|codex|none",
                    agent: AgentType::Codex,
                },
                SkillRoute {
                    work_unit_key: "task|4|reviewer|primary|codex|none",
                    agent: AgentType::Codex,
                },
                SkillRoute {
                    work_unit_key: "task|4|reviewer|auxiliary|codex|none",
                    agent: AgentType::Codex,
                },
            ],
            expected_actions: &[SkillAction::FreshDelegate],
            must_differ_from: &[],
            max_unexpected_continues: 2,
            max_replacements: 1,
        },
        SkillScenario {
            name: "high_fix_and_both_rereviews_continue",
            routes: &[
                SkillRoute {
                    work_unit_key: "task|5|implementer|codex|none",
                    agent: AgentType::Codex,
                },
                SkillRoute {
                    work_unit_key: "task|5|reviewer|primary|codex|none",
                    agent: AgentType::Codex,
                },
                SkillRoute {
                    work_unit_key: "task|5|reviewer|auxiliary|grok|none",
                    agent: AgentType::Grok,
                },
            ],
            expected_actions: &[SkillAction::Continue],
            must_differ_from: &[],
            max_unexpected_continues: 2,
            max_replacements: 1,
        },
        SkillScenario {
            name: "conditional_design_reviewer_and_fixer_are_separate",
            routes: &[
                SkillRoute {
                    work_unit_key: "design|docs/design.md|reviewer|codex|none",
                    agent: AgentType::Codex,
                },
                SkillRoute {
                    work_unit_key: "design|docs/design.md|fixer|codex|none",
                    agent: AgentType::Codex,
                },
            ],
            expected_actions: &[SkillAction::Continue],
            must_differ_from: &[],
            max_unexpected_continues: 2,
            max_replacements: 1,
        },
        SkillScenario {
            name: "plan_author_and_reviewer_stay_separate",
            routes: &[
                SkillRoute {
                    work_unit_key: "plan|docs/plan.md|author|codex|none",
                    agent: AgentType::Codex,
                },
                SkillRoute {
                    work_unit_key: "plan|docs/plan.md|reviewer|codex|none",
                    agent: AgentType::Codex,
                },
            ],
            expected_actions: &[SkillAction::Continue],
            must_differ_from: &[],
            max_unexpected_continues: 2,
            max_replacements: 1,
        },
        SkillScenario {
            name: "boundary_agent_change_affects_pending_tasks",
            routes: &[
                SkillRoute {
                    work_unit_key: "task|7|implementer|grok|none",
                    agent: AgentType::Grok,
                },
                SkillRoute {
                    work_unit_key: "task|8|implementer|gemini|careful",
                    agent: AgentType::Gemini,
                },
            ],
            expected_actions: &[SkillAction::FreshDelegate],
            must_differ_from: &[],
            max_unexpected_continues: 2,
            max_replacements: 1,
        },
        SkillScenario {
            name: "active_task_change_blocks_without_handoff",
            routes: &[SkillRoute {
                work_unit_key: "task|9|implementer|grok|none",
                agent: AgentType::Grok,
            }],
            expected_actions: &[SkillAction::BlockNoSubstitute],
            must_differ_from: &[],
            max_unexpected_continues: 2,
            max_replacements: 1,
        },
        SkillScenario {
            name: "recovery_keeps_agent_profile_key_and_budgets",
            routes: &[SkillRoute {
                work_unit_key: "task|10|implementer|gemini|careful",
                agent: AgentType::Gemini,
            }],
            expected_actions: &[SkillAction::Continue, SkillAction::Replacement],
            must_differ_from: &[],
            max_unexpected_continues: 2,
            max_replacements: 1,
        },
        SkillScenario {
            name: "final_findings_return_to_owners_and_reopen_reviews",
            routes: &[
                SkillRoute {
                    work_unit_key: "task|11|implementer|grok|none",
                    agent: AgentType::Grok,
                },
                SkillRoute {
                    work_unit_key: "task|11|reviewer|primary|codex|none",
                    agent: AgentType::Codex,
                },
                SkillRoute {
                    work_unit_key: "task|12|implementer|codex|none",
                    agent: AgentType::Codex,
                },
                SkillRoute {
                    work_unit_key: "task|12|reviewer|primary|codex|none",
                    agent: AgentType::Codex,
                },
                SkillRoute {
                    work_unit_key: "task|12|reviewer|auxiliary|grok|none",
                    agent: AgentType::Grok,
                },
                SkillRoute {
                    work_unit_key: "final_review|reviewer|codex|none",
                    agent: AgentType::Codex,
                },
            ],
            expected_actions: &[SkillAction::Continue],
            must_differ_from: &[],
            max_unexpected_continues: 2,
            max_replacements: 1,
        },
    ]
}

#[test]
fn skill_forward_routing_invariants_eleven_v2_scenarios() {
    let scenarios = skill_forward_v2_scenarios();
    assert_eq!(scenarios.len(), 11);
    assert_eq!(
        scenarios
            .iter()
            .map(|scenario| scenario.name)
            .collect::<Vec<_>>(),
        vec![
            "default_normal_grok_plus_codex_primary",
            "selected_non_grok_normal_route",
            "high_codex_plus_primary_and_task_agent_auxiliary",
            "codex_task_agent_keeps_three_distinct_units",
            "high_fix_and_both_rereviews_continue",
            "conditional_design_reviewer_and_fixer_are_separate",
            "plan_author_and_reviewer_stay_separate",
            "boundary_agent_change_affects_pending_tasks",
            "active_task_change_blocks_without_handoff",
            "recovery_keeps_agent_profile_key_and_budgets",
            "final_findings_return_to_owners_and_reopen_reviews",
        ]
    );

    for scenario in scenarios {
        assert!(!scenario.routes.is_empty(), "{}", scenario.name);
        assert!(scenario.max_unexpected_continues <= UNEXPECTED_CONTINUE_LIMIT);
        assert!(scenario.max_replacements <= REPLACEMENT_LIMIT);
        let keys = scenario
            .routes
            .iter()
            .map(|route| route.work_unit_key)
            .collect::<BTreeSet<_>>();
        assert_eq!(keys.len(), scenario.routes.len(), "{}", scenario.name);

        let child_by_key = scenario
            .routes
            .iter()
            .enumerate()
            .map(|(index, route)| (route.work_unit_key, index + 1))
            .collect::<HashMap<_, _>>();
        assert_eq!(child_by_key.len(), keys.len(), "{}", scenario.name);
        for left in scenario.routes {
            assert!(
                left.work_unit_key.chars().count() <= 200,
                "{} key too long",
                scenario.name
            );
            assert!(
                left.work_unit_key
                    .split('|')
                    .any(|part| part == left.agent.as_wire().as_ref()),
                "{} key must encode its Agent",
                scenario.name
            );
            for prior_key in scenario.must_differ_from {
                assert_ne!(left.work_unit_key, *prior_key, "{}", scenario.name);
            }
            for right in scenario.routes {
                if left.work_unit_key != right.work_unit_key {
                    assert_ne!(
                        child_by_key[left.work_unit_key], child_by_key[right.work_unit_key],
                        "distinct route keys cannot share a child"
                    );
                }
            }
        }
        for action in scenario.expected_actions {
            match action {
                SkillAction::Continue => assert!(!request_fingerprint(
                    CONTINUE_DELEGATION_TOOL,
                    "follow-up",
                    Some(scenario.routes[0].work_unit_key),
                    None,
                    None,
                    Some("prior-task"),
                    "deadbeef",
                    None,
                )
                .is_empty()),
                SkillAction::FreshDelegate => assert!(!request_fingerprint(
                    DELEGATE_TO_AGENT_TOOL,
                    "fresh task",
                    Some(scenario.routes[0].work_unit_key),
                    None,
                    None,
                    None,
                    "deadbeef",
                    None,
                )
                .is_empty()),
                SkillAction::Replacement => assert!(!request_fingerprint(
                    DELEGATE_TO_AGENT_TOOL,
                    "replacement",
                    Some(scenario.routes[0].work_unit_key),
                    Some("failed-task"),
                    Some(REPLACEMENT_REASON_UNRESUMABLE),
                    None,
                    "deadbeef",
                    None,
                )
                .is_empty()),
                SkillAction::BlockNoSubstitute => {
                    assert_ne!("route_policy", REPLACEMENT_REASON_UNRESUMABLE)
                }
            }
        }
    }
}

#[test]
fn skill_forward_contract_v2_matches_skill() {
    let skill_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(".agents")
        .join("skills")
        .join("brainstorm-to-delivery")
        .join("SKILL.md");
    let skill = std::fs::read_to_string(&skill_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", skill_path.display()));
    const CONTRACT_MARKER: &str = "<!-- codeg-b2d-skill-contract-v2";
    let mut contract_markers = skill.match_indices(CONTRACT_MARKER);
    let (contract_start, _) = contract_markers.next().unwrap_or_else(|| {
        panic!(
            "{} is missing structured contract marker `{CONTRACT_MARKER}`",
            skill_path.display()
        )
    });
    assert!(
        contract_markers.next().is_none(),
        "{} must contain exactly one `{CONTRACT_MARKER}` comment",
        skill_path.display()
    );
    let contract_body = &skill[contract_start + CONTRACT_MARKER.len()..];
    let contract_end = contract_body.find("-->").unwrap_or_else(|| {
        panic!(
            "{} structured contract is missing its `-->` terminator",
            skill_path.display()
        )
    });
    let contract: serde_json::Value = serde_json::from_str(contract_body[..contract_end].trim())
        .unwrap_or_else(|error| {
            panic!(
                "parse structured contract JSON in {}: {error}",
                skill_path.display()
            )
        });

    assert_eq!(
        contract["phase_order"],
        serde_json::json!([
            "establish-current-truth",
            "resolve-task-agent",
            "review-and-revise-design",
            "author-and-review-plan",
            "maintain-progress",
            "apply-workspace-gate",
            "execute-tasks-serially",
            "recover-generic-runs",
            "complete-final-review"
        ]),
        "structured contract phase_order"
    );
    assert_eq!(
        [
            contract["interfaces"]["first_run"].as_str(),
            contract["interfaces"]["later_run"].as_str(),
            contract["interfaces"]["join"].as_str(),
        ],
        [
            Some("delegate_to_agent"),
            Some("continue_delegation"),
            Some("get_delegation_status"),
        ],
        "structured contract delegation interfaces"
    );
    assert_eq!(
        contract["routing"],
        serde_json::json!({
            "marker": "codeg-b2d-routing-v1",
            "risk_policy_version": "b2d_task_risk_v1",
            "normal": {
                "implementer": "task_agent",
                "reviewers": ["codex_primary"]
            },
            "high": {
                "implementer": "codex",
                "reviewers": ["codex_primary", "task_agent_auxiliary"]
            },
            "reviewer_slots": ["primary", "auxiliary"],
            "task_order": "serial",
            "high_review_fan_out": "parallel_after_implementation"
        }),
        "structured contract routing"
    );
    assert_eq!(
        contract["recovery"],
        serde_json::json!({
            "unexpected_continuations": 2,
            "logical_replacements": 1,
            "replacement_retry": "pre-admission-only"
        }),
        "structured contract recovery limits"
    );
    assert_eq!(
        contract["final_review"],
        serde_json::json!({
            "required": true,
            "independent": true,
            "reviewer": "codex",
            "fix_owner": "task_producer"
        }),
        "structured contract final review"
    );
}

#[tokio::test]
async fn skill_forward_rereviews_continue_same_owned_sessions() {
    let db = Arc::new(fresh_in_memory_db().await);
    let folder = seed_folder(&db, "/tmp/codeg-skill-rereviews").await;
    let parent = conversation_service::create(
        &db.conn,
        folder,
        AgentType::ClaudeCode,
        Some("skill rereview parent".into()),
        None,
    )
    .await
    .expect("parent");
    let runs = Arc::new(RunStore::new(db.clone()));
    let mock = Arc::new(MockSpawner::new());
    let broker = broker_with_run_store(mock.clone(), parent.id, runs.clone()).await;
    let workdir = "/tmp/codeg-skill-rereviews";

    // Scenarios 1-3: document re-reviews and Task fix/re-review must resume
    // the exact reviewer/implementer session, never route across work units.
    let routes = [
        (
            "design-review",
            "design|docs/codeg-design.md|reviewer|codex|none",
            AgentType::Codex,
        ),
        (
            "design-fixer",
            "design|docs/codeg-design.md|fixer|codex|none",
            AgentType::Codex,
        ),
        (
            "plan-author",
            "plan|docs/codeg-plan.md|author|codex|none",
            AgentType::Codex,
        ),
        (
            "plan-review",
            "plan|docs/codeg-plan.md|reviewer|codex|none",
            AgentType::Codex,
        ),
        (
            "task-3-implementer",
            "task|3|implementer|codex|none",
            AgentType::Codex,
        ),
        (
            "task-3-reviewer",
            "task|3|reviewer|primary|codex|none",
            AgentType::Codex,
        ),
        (
            "task-3-auxiliary",
            "task|3|reviewer|auxiliary|grok|none",
            AgentType::Grok,
        ),
    ];
    let route_count = routes.len();
    let mut children = HashMap::new();

    for (name, work_unit_key, agent) in routes {
        let initial_tool_use_id = format!("tu-skill-{name}-initial");
        let initial_task = format!("initial {name}");
        let initial_connection = format!("skill-{name}-initial");
        let (initial_task_id, child_id) = start_and_complete(
            &broker,
            &mock,
            delegate_req(
                parent.id,
                &initial_tool_use_id,
                agent,
                &initial_task,
                workdir,
                Some(work_unit_key),
            ),
            &initial_connection,
        )
        .await;
        set_child_external_id(&db, child_id, &format!("sess-skill-{name}")).await;

        let continue_tool_use_id = format!("tu-skill-{name}-continue");
        let follow_up = format!("follow up {name}");
        let continue_connection = format!("skill-{name}-continue");
        let continued_task_id = continue_and_complete(
            &broker,
            &mock,
            continue_req(
                parent.id,
                &continue_tool_use_id,
                &initial_task_id,
                &follow_up,
                Some(work_unit_key),
            ),
            &continue_connection,
            child_id,
            agent,
        )
        .await;
        let continued = runs
            .load_by_task_id(&continued_task_id)
            .await
            .expect("continued run lookup")
            .expect("continued run");
        assert_eq!(
            continued.previous_task_id.as_deref(),
            Some(initial_task_id.as_str())
        );
        assert_eq!(continued.child_conversation_id, child_id);
        assert_eq!(continued.agent_type, agent);
        children.insert(name, child_id);
    }

    let unique_children = children.values().copied().collect::<BTreeSet<_>>();
    assert_eq!(
        unique_children.len(),
        route_count,
        "work units stay isolated"
    );
    assert_ne!(children["design-review"], children["plan-review"]);
    assert_ne!(children["design-review"], children["design-fixer"]);
    assert_ne!(children["plan-author"], children["plan-review"]);
    assert_ne!(children["task-3-implementer"], children["task-3-reviewer"]);
    assert_ne!(children["task-3-reviewer"], children["task-3-auxiliary"]);
}

#[tokio::test]
async fn skill_forward_new_task_and_final_review_start_fresh_sessions() {
    let db = Arc::new(fresh_in_memory_db().await);
    let folder = seed_folder(&db, "/tmp/codeg-skill-fresh").await;
    let parent = conversation_service::create(
        &db.conn,
        folder,
        AgentType::ClaudeCode,
        Some("skill fresh parent".into()),
        None,
    )
    .await
    .expect("parent");
    let runs = Arc::new(RunStore::new(db.clone()));
    let mock = Arc::new(MockSpawner::new());
    let broker = broker_with_run_store(mock.clone(), parent.id, runs).await;
    let workdir = "/tmp/codeg-skill-fresh";

    // Scenarios 4-5: a new Task gets fresh Grok/Codex children and final
    // whole-branch review starts a fresh Codex child rather than Task review.
    let routes = [
        (
            "task-3-implementer",
            "task|3|implementer|grok|none",
            AgentType::Grok,
        ),
        (
            "task-3-reviewer",
            "task|3|reviewer|primary|codex|none",
            AgentType::Codex,
        ),
        (
            "task-4-implementer",
            "task|4|implementer|grok|none",
            AgentType::Grok,
        ),
        (
            "task-4-reviewer",
            "task|4|reviewer|primary|codex|none",
            AgentType::Codex,
        ),
        (
            "final-review",
            "final_review|reviewer|codex|none",
            AgentType::Codex,
        ),
        (
            "task-5-implementer",
            "task|5|implementer|codex|none",
            AgentType::Codex,
        ),
        (
            "task-5-primary",
            "task|5|reviewer|primary|codex|none",
            AgentType::Codex,
        ),
        (
            "task-5-auxiliary",
            "task|5|reviewer|auxiliary|codex|none",
            AgentType::Codex,
        ),
    ];
    let route_count = routes.len();
    let mut children = HashMap::new();

    for (name, work_unit_key, agent) in routes {
        let tool_use_id = format!("tu-skill-fresh-{name}");
        let task = format!("fresh {name}");
        let connection = format!("skill-fresh-{name}");
        let (_, child_id) = start_and_complete(
            &broker,
            &mock,
            delegate_req(
                parent.id,
                &tool_use_id,
                agent,
                &task,
                workdir,
                Some(work_unit_key),
            ),
            &connection,
        )
        .await;
        children.insert(name, child_id);
    }

    let unique_children = children.values().copied().collect::<BTreeSet<_>>();
    assert_eq!(
        unique_children.len(),
        route_count,
        "every fresh route owns a child"
    );
    assert_ne!(
        children["task-3-implementer"],
        children["task-4-implementer"]
    );
    assert_ne!(children["task-3-reviewer"], children["task-4-reviewer"]);
    assert_ne!(children["task-3-reviewer"], children["final-review"]);
    assert_ne!(children["task-5-implementer"], children["task-5-primary"]);
    assert_ne!(children["task-5-primary"], children["task-5-auxiliary"]);
}

#[tokio::test]
async fn skill_forward_business_error_does_not_spawn_substitute() {
    let db = Arc::new(fresh_in_memory_db().await);
    let folder = seed_folder(&db, "/tmp/codeg-skill-business-error").await;
    let parent = conversation_service::create(
        &db.conn,
        folder,
        AgentType::ClaudeCode,
        Some("skill business error parent".into()),
        None,
    )
    .await
    .expect("parent");
    let runs = Arc::new(RunStore::new(db.clone()));
    let mock = Arc::new(MockSpawner::new());
    let broker = broker_with_run_store(mock.clone(), parent.id, runs.clone()).await;
    let work_unit_key = "task|6|reviewer|primary|codex|none";

    let (source_task_id, _) = start_and_complete(
        &broker,
        &mock,
        delegate_req(
            parent.id,
            "tu-skill-business-source",
            AgentType::Codex,
            "review task six",
            "/tmp/codeg-skill-business-error",
            Some(work_unit_key),
        ),
        "skill-business-source",
    )
    .await;
    let spawn_count_before = mock.spawn_args.lock().await.len();

    // Scenario 8: a business/lifecycle error is not an approved replacement
    // reason, so the broker must reject before it creates a substitute agent.
    let mut prohibited = delegate_req(
        parent.id,
        "tu-skill-business-substitute",
        AgentType::Codex,
        "do not substitute on busy thread",
        "/tmp/codeg-skill-business-error",
        Some(work_unit_key),
    );
    prohibited.replaces_task_id = Some(source_task_id);
    prohibited.replacement_reason = Some("busy_thread".into());
    let report = broker.start_delegation(prohibited).await;
    assert_eq!(report.status, TaskStatus::Failed, "{report:?}");
    assert_eq!(report.error_code.as_deref(), Some("invalid_replacement"));
    assert_eq!(mock.spawn_args.lock().await.len(), spawn_count_before);
    assert_eq!(list_run_rows(&db, parent.id).await.len(), 1);
}

// ---------------------------------------------------------------------------
// 5. Concurrent double-continue → one winner, loser busy_thread
// ---------------------------------------------------------------------------

#[tokio::test]
async fn concurrent_double_continue_one_winner_busy_thread() {
    let db = Arc::new(fresh_in_memory_db().await);
    let folder = seed_folder(&db, "/tmp/codeg-double-continue").await;
    let parent = conversation_service::create(
        &db.conn,
        folder,
        AgentType::ClaudeCode,
        Some("double-continue parent".into()),
        None,
    )
    .await
    .expect("parent");
    let runs = Arc::new(RunStore::new(db.clone()));
    let mock = Arc::new(MockSpawner::new());
    // Only one continue may resume; a second spawn would fail loudly if the
    // durable fence did not stop the loser.
    mock.queue_spawn(Ok("dc-root".into())).await;
    mock.queue_send(Ok(accepted(0, Utc::now()))).await;
    mock.queue_spawn(Ok("dc-continue-winner".into())).await;
    mock.queue_send(Ok(accepted(0, Utc::now()))).await;

    let broker = broker_with_run_store(mock.clone(), parent.id, runs.clone()).await;
    let root = broker
        .start_delegation(delegate_req(
            parent.id,
            "tu-dc-root",
            AgentType::Codex,
            "root",
            "/tmp/codeg-double-continue",
            Some("unit-dc"),
        ))
        .await;
    let root_id = root.task_id.expect("root");
    let child_id = root.child_conversation_id.expect("child");
    broker
        .complete_call(
            &root_id,
            completed_outcome("done", child_id, AgentType::Codex),
        )
        .await;
    set_child_external_id(&db, child_id, "sess-dc").await;

    let barrier = Arc::new(Barrier::new(2));
    let b1 = barrier.clone();
    let br1 = broker.clone();
    let t1 = {
        let root_id = root_id.clone();
        let parent_id = parent.id;
        tokio::spawn(async move {
            b1.wait().await;
            br1.continue_delegation(continue_req(
                parent_id,
                "tu-dc-a",
                &root_id,
                "continue A",
                Some("unit-dc"),
            ))
            .await
        })
    };
    let b2 = barrier.clone();
    let br2 = broker.clone();
    let t2 = {
        let root_id = root_id.clone();
        let parent_id = parent.id;
        tokio::spawn(async move {
            b2.wait().await;
            br2.continue_delegation(continue_req(
                parent_id,
                "tu-dc-b",
                &root_id,
                "continue B",
                Some("unit-dc"),
            ))
            .await
        })
    };

    let (r1, r2) = tokio::join!(t1, t2);
    let r1 = r1.expect("join a");
    let r2 = r2.expect("join b");
    let running =
        (r1.status == TaskStatus::Running) as u8 + (r2.status == TaskStatus::Running) as u8;
    let busy = (r1.error_code.as_deref() == Some("busy_thread")) as u8
        + (r2.error_code.as_deref() == Some("busy_thread")) as u8;
    assert_eq!(running, 1, "exactly one winner: {r1:?} / {r2:?}");
    assert_eq!(busy, 1, "loser busy_thread: {r1:?} / {r2:?}");

    // ResumeExistingOnly path used at most once for the winner.
    let resumes = mock.resume_args.lock().await.len();
    assert_eq!(resumes, 1, "only winner resumes; resumes={resumes}");
}

// ---------------------------------------------------------------------------
// 6. ResumeExistingOnly — reuses session; missing external id is not continuable
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resume_existing_only_reuses_session_and_records_resume_call() {
    let db = Arc::new(fresh_in_memory_db().await);
    let folder = seed_folder(&db, "/tmp/codeg-resume-only").await;
    let parent = conversation_service::create(
        &db.conn,
        folder,
        AgentType::ClaudeCode,
        Some("resume-only parent".into()),
        None,
    )
    .await
    .expect("parent");
    let runs = Arc::new(RunStore::new(db.clone()));
    let mock = Arc::new(MockSpawner::new());
    let broker = broker_with_run_store(mock.clone(), parent.id, runs.clone()).await;

    let (root_id, child_id) = start_and_complete(
        &broker,
        &mock,
        delegate_req(
            parent.id,
            "tu-ro-root",
            AgentType::Codex,
            "root",
            "/tmp/codeg-resume-only",
            None,
        ),
        "ro-root",
    )
    .await;
    set_child_external_id(&db, child_id, "external-session-abc").await;

    let before_children = conversation_service::list_children(&db.conn, parent.id)
        .await
        .unwrap()
        .len();
    let resume_before = mock.resume_args.lock().await.len();

    mock.queue_spawn(Ok("ro-continue".into())).await;
    mock.queue_send(Ok(accepted(child_id, Utc::now()))).await;
    let cont = broker
        .continue_delegation(continue_req(
            parent.id,
            "tu-ro-cont",
            &root_id,
            "continue resume only",
            None,
        ))
        .await;
    assert_eq!(cont.status, TaskStatus::Running, "{cont:?}");
    assert_eq!(cont.reused_session, Some(true));
    assert_eq!(cont.child_conversation_id, Some(child_id));

    let resumes = mock.resume_args.lock().await;
    assert_eq!(resumes.len(), resume_before + 1);
    let last = resumes.last().expect("resume recorded");
    assert_eq!(last.external_session_id, "external-session-abc");
    assert!(last.preallocated_connection_id.is_some());
    drop(resumes);

    let after_children = conversation_service::list_children(&db.conn, parent.id)
        .await
        .unwrap()
        .len();
    assert_eq!(
        after_children, before_children,
        "ResumeExistingOnly must not create a new child conversation"
    );
}

#[tokio::test]
async fn cursor_resume_existing_only_reuses_session_and_records_resume_call() {
    let db = Arc::new(fresh_in_memory_db().await);
    let folder = seed_folder(&db, "/tmp/codeg-cursor-resume-only").await;
    let parent = conversation_service::create(
        &db.conn,
        folder,
        AgentType::ClaudeCode,
        Some("cursor resume-only parent".into()),
        None,
    )
    .await
    .expect("parent");
    let runs = Arc::new(RunStore::new(db.clone()));
    let mock = Arc::new(MockSpawner::new());
    let broker = broker_with_run_store(mock.clone(), parent.id, runs.clone()).await;

    let (root_id, child_id) = start_and_complete(
        &broker,
        &mock,
        delegate_req(
            parent.id,
            "tu-cursor-resume-root",
            AgentType::Cursor,
            "root",
            "/tmp/codeg-cursor-resume-only",
            None,
        ),
        "cursor-resume-root",
    )
    .await;
    set_child_external_id(&db, child_id, "cursor-session-abc").await;

    let projection = runs
        .recovery_projection_for_task(&root_id)
        .await
        .expect("cursor recovery projection")
        .expect("cursor run must be recoverable");
    assert_eq!(projection.disposition, "continue", "{projection:?}");
    assert_eq!(projection.proposed_action.as_deref(), Some("continue"));

    let before_children = conversation_service::list_children(&db.conn, parent.id)
        .await
        .unwrap()
        .len();
    mock.queue_spawn(Ok("cursor-resume-continue".into())).await;
    mock.queue_send(Ok(accepted(child_id, Utc::now()))).await;

    let cont = broker
        .continue_delegation(continue_req(
            parent.id,
            "tu-cursor-resume-cont",
            &root_id,
            "continue Cursor session",
            None,
        ))
        .await;
    assert_eq!(cont.status, TaskStatus::Running, "{cont:?}");
    assert_eq!(cont.agent_type, Some(AgentType::Cursor));
    assert_eq!(cont.reused_session, Some(true));
    assert_eq!(cont.child_conversation_id, Some(child_id));

    let resumes = mock.resume_args.lock().await;
    let last = resumes.last().expect("Cursor resume recorded");
    assert_eq!(last.external_session_id, "cursor-session-abc");
    assert!(last.preallocated_connection_id.is_some());
    drop(resumes);

    let after_children = conversation_service::list_children(&db.conn, parent.id)
        .await
        .unwrap()
        .len();
    assert_eq!(
        after_children, before_children,
        "Cursor continuation must not create a new child conversation"
    );
}

#[tokio::test]
async fn resume_existing_only_connection_id_mismatch_is_unresumable() {
    let db = Arc::new(fresh_in_memory_db().await);
    let folder = seed_folder(&db, "/tmp/codeg-resume-mismatch").await;
    let parent = conversation_service::create(
        &db.conn,
        folder,
        AgentType::ClaudeCode,
        Some("resume mismatch parent".into()),
        None,
    )
    .await
    .expect("parent");
    let runs = Arc::new(RunStore::new(db.clone()));
    let mock = Arc::new(MockSpawner::new());
    let broker = broker_with_run_store(mock.clone(), parent.id, runs.clone()).await;

    let (root_id, child_id) = start_and_complete(
        &broker,
        &mock,
        delegate_req(
            parent.id,
            "tu-rmismatch-root",
            AgentType::Codex,
            "root",
            "/tmp/codeg-resume-mismatch",
            None,
        ),
        "rmismatch-root",
    )
    .await;
    set_child_external_id(&db, child_id, "external-session-mismatch").await;
    let child_count = conversation_service::list_children(&db.conn, parent.id)
        .await
        .unwrap()
        .len();

    // The mock's normal inner spawn remains successful, while the explicit
    // resume return violates the preallocated handoff incarnation.
    mock.queue_spawn(Ok("ignored-resume-spawn-id".into())).await;
    mock.queue_resume(Ok("unexpected-resume-connection".into()))
        .await;
    // A queued send proves that the broker rejects before prompt enqueue.
    mock.queue_send(Err(
        codeg_lib::acp::delegation::spawner::SpawnerError::send("must not send"),
    ))
    .await;

    let report = broker
        .continue_delegation(continue_req(
            parent.id,
            "tu-rmismatch-cont",
            &root_id,
            "resume must reject mismatched incarnation",
            None,
        ))
        .await;
    assert_eq!(report.status, TaskStatus::Failed, "{report:?}");
    assert_eq!(report.error_code.as_deref(), Some("unresumable"));
    assert_eq!(report.child_conversation_id, Some(child_id));
    assert_eq!(
        report.continued_from_task_id.as_deref(),
        Some(root_id.as_str()),
        "durably reserved continuation must retain its predecessor"
    );

    let failed_id = report
        .task_id
        .clone()
        .unwrap_or_else(|| panic!("failed continuation report omitted task id: {report:?}"));
    let failed = runs
        .load_by_task_id(&failed_id)
        .await
        .unwrap()
        .expect("failed run");
    assert_eq!(failed.run_status, DelegationRunStatus::Failed);
    assert_eq!(failed.error_code.as_deref(), Some("unresumable"));
    assert!(
        mock.disconnects
            .lock()
            .await
            .contains(&"unexpected-resume-connection".to_string()),
        "resume mismatch must disconnect the unexpected incarnation"
    );
    assert_eq!(mock.send_results.lock().await.len(), 1, "no prompt enqueue");

    let children_after = conversation_service::list_children(&db.conn, parent.id)
        .await
        .unwrap()
        .len();
    assert_eq!(
        children_after, child_count,
        "resume mismatch must not create a child"
    );
}

#[tokio::test]
async fn resume_existing_only_missing_external_id_is_not_continuable() {
    let db = Arc::new(fresh_in_memory_db().await);
    let folder = seed_folder(&db, "/tmp/codeg-resume-missing-ext").await;
    let parent = conversation_service::create(
        &db.conn,
        folder,
        AgentType::ClaudeCode,
        Some("resume-missing parent".into()),
        None,
    )
    .await
    .expect("parent");
    let runs = Arc::new(RunStore::new(db.clone()));
    let mock = Arc::new(MockSpawner::new());
    let broker = broker_with_run_store(mock.clone(), parent.id, runs).await;

    let (root_id, _child_id) = start_and_complete(
        &broker,
        &mock,
        delegate_req(
            parent.id,
            "tu-rm-root",
            AgentType::Codex,
            "root",
            "/tmp/codeg-resume-missing-ext",
            None,
        ),
        "rm-root",
    )
    .await;
    // Intentionally leave external_id unset. This is a durable eligibility
    // failure, so it must not try ResumeExistingOnly or fall through to new.

    let report = broker
        .continue_delegation(continue_req(
            parent.id,
            "tu-rm-cont",
            &root_id,
            "cannot resume",
            None,
        ))
        .await;
    assert_eq!(
        report.error_code.as_deref(),
        Some("not_continuable"),
        "{report:?}"
    );
    assert!(
        mock.resume_args.lock().await.is_empty(),
        "missing external id must fail before ResumeExistingOnly spawn"
    );
}

// ---------------------------------------------------------------------------
// 7. Preview redaction + summary non-exposure (+ migration collision smoke)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn task_preview_redaction_and_summary_not_in_parent_mcp_report() {
    let db = Arc::new(fresh_in_memory_db().await);
    let folder = seed_folder(&db, "/tmp/codeg-preview-summary").await;
    let parent = conversation_service::create(
        &db.conn,
        folder,
        AgentType::ClaudeCode,
        Some("preview parent".into()),
        None,
    )
    .await
    .expect("parent");
    let runs = Arc::new(RunStore::new(db.clone()));
    let mock = Arc::new(MockSpawner::new());
    let broker = broker_with_run_store(mock.clone(), parent.id, runs.clone()).await;

    let secret_task =
        "Review with token Bearer sk-live-super-secret-value-please-hide and ghp_abcdefghijklmnopqrstuv";
    mock.queue_spawn(Ok("ps-root".into())).await;
    mock.queue_send(Ok(accepted(0, Utc::now()))).await;
    let ack = broker
        .start_delegation(delegate_req(
            parent.id,
            "tu-ps-root",
            AgentType::Codex,
            secret_task,
            "/tmp/codeg-preview-summary",
            None,
        ))
        .await;
    let task_id = ack.task_id.expect("task");
    let child_id = ack.child_conversation_id.expect("child");

    let run = runs.load_by_task_id(&task_id).await.unwrap().expect("run");
    let preview = run.task_preview.as_deref().unwrap_or("");
    assert!(
        preview.contains("[redacted]"),
        "preview must redact secrets: {preview}"
    );
    assert!(
        !preview.contains("sk-live-super-secret"),
        "raw secret must not persist in task_preview"
    );
    assert_eq!(
        preview,
        derive_task_preview(secret_task),
        "broker preview must match derive_task_preview"
    );

    let summary_block = format!(
        r#"Review body ok.
{CARD_SUMMARY_MARKER}
{{"kind":"review","verdict":"approve","critical":0,"important":0,"minor":1,"summary":"Looks good"}}
-->
"#
    );
    assert!(extract_card_summary(&summary_block).is_some());
    broker
        .complete_call(
            &task_id,
            completed_outcome(&summary_block, child_id, AgentType::Codex),
        )
        .await;

    let status = broker
        .get_task_status(
            "parent-conn",
            Some(parent.id),
            &task_id,
            StatusWait::Snapshot,
        )
        .await;
    assert_eq!(status.status, TaskStatus::Completed, "{status:?}");
    let text = status.text.as_deref().unwrap_or("");
    assert!(
        !text.contains(CARD_SUMMARY_MARKER),
        "card summary must not appear in parent MCP result text: {text}"
    );
    assert!(
        !text.contains("Looks good"),
        "card-summary text must not appear in parent MCP result: {text}"
    );
    assert!(!text.contains("\"verdict\":\"approve\""));

    // Snapshot DTO still surfaces validated card summary for the frontend card.
    let snap = get_delegation_run_snapshot_core(&db.conn, parent.id, &task_id)
        .await
        .expect("snapshot");
    assert!(
        snap.card_summary.is_some(),
        "frontend snapshot keeps summary"
    );
}

#[tokio::test]
async fn migration_collision_unique_parent_tool_losers_null_key() {
    // Contract smoke: post-migration unique parent-tool key still allows
    // multiple historical rows when the unique key is NULL for losers
    // (full migration suite lives in delegation_task_runs_migration.rs).
    use sea_orm::{ConnectionTrait, DbBackend, Statement};

    let db = fresh_in_memory_db().await;
    db.conn
        .execute(Statement::from_string(
            DbBackend::Sqlite,
            "INSERT INTO folder \
             (id,name,path,last_opened_at,created_at,updated_at,is_open,sort_order,color,kind) \
             VALUES (1,'repo','/tmp/mig-coll','2026-07-21','2026-07-21','2026-07-21',1,1,'inherit','regular')"
                ,
        ))
        .await
        .unwrap();
    db.conn
        .execute(Statement::from_string(
            DbBackend::Sqlite,
            "INSERT INTO conversation \
             (id,folder_id,agent_type,status,kind,message_count,title_locked,auto_title_finalized,created_at,updated_at) \
             VALUES (10,1,'codex','completed','regular',0,0,0,'2026-07-21','2026-07-21')"
                ,
        ))
        .await
        .unwrap();
    for (task_id, child, parent_tool_use_id, legacy_parent_tool_use_id) in [
        ("run-win", 20, "'tool-shared'", "NULL"),
        ("run-lose", 21, "NULL", "'tool-shared'"),
    ] {
        db.conn
            .execute(Statement::from_string(
                DbBackend::Sqlite,
                format!(
                    "INSERT INTO conversation \
                     (id,folder_id,agent_type,status,kind,message_count,title_locked,auto_title_finalized,created_at,updated_at,parent_id) \
                     VALUES ({child},1,'codex','completed','delegate',0,0,0,'2026-07-21','2026-07-21',10)"
                ),
            ))
            .await
            .unwrap();
        db.conn
            .execute(Statement::from_string(
                DbBackend::Sqlite,
                format!(
                    "INSERT INTO delegation_task_runs \
                     (task_id,root_task_id,previous_task_id,generation,parent_conversation_id,parent_tool_use_id,child_conversation_id,agent_type,admission_class,lineage_root_task_id,legacy_parent_tool_use_id,history_only,status,created_at,updated_at,tool_call_count,edit_tool_call_count,touched_files_json,touched_files_truncated,additions,deletions,line_counts_complete) \
                     VALUES ('{task_id}','{task_id}',NULL,1,10,{parent_tool_use_id},{child},'codex','normal_revision','{task_id}',{legacy_parent_tool_use_id},1,'completed','2026-07-21T00:00:00Z','2026-07-21T00:00:00Z',0,0,'[]',0,0,0,1)"
                ),
            ))
            .await
            .expect("insert with NULL unique key for collision loser must succeed");
    }

    let count: i64 = db
        .conn
        .query_one(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT COUNT(*) AS c FROM delegation_task_runs \
             WHERE parent_tool_use_id='tool-shared' OR legacy_parent_tool_use_id='tool-shared'",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "c")
        .unwrap();
    assert_eq!(count, 2);

    let winner = DelegationTaskRun::find_by_id("run-win")
        .one(&db.conn)
        .await
        .unwrap()
        .expect("winner");
    assert_eq!(winner.parent_tool_use_id.as_deref(), Some("tool-shared"));
    assert_eq!(winner.legacy_parent_tool_use_id, None);

    let loser = DelegationTaskRun::find_by_id("run-lose")
        .one(&db.conn)
        .await
        .unwrap()
        .expect("loser");
    assert_eq!(loser.parent_tool_use_id, None);
    assert_eq!(
        loser.legacy_parent_tool_use_id.as_deref(),
        Some("tool-shared")
    );
    assert!(loser.history_only);
}

// ---------------------------------------------------------------------------
// 8. Pre-admission re-dispatch + replacement retry (never-running priors)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pre_admission_host_restart_allows_fresh_gen1_redispatch() {
    let db = Arc::new(fresh_in_memory_db().await);
    let folder = seed_folder(&db, "/tmp/codeg-preadmit-fresh").await;
    let parent = conversation_service::create(
        &db.conn,
        folder,
        AgentType::ClaudeCode,
        Some("pre-admission fresh parent".into()),
        None,
    )
    .await
    .expect("parent");
    let source_task_id = "pre-admission-fresh-source";
    let old_child = conversation_service::create_with_delegation(
        &db.conn,
        folder,
        AgentType::Grok,
        Some("pre-admission fresh child".into()),
        None,
        Some(DelegationLink {
            parent_conversation_id: parent.id,
            parent_tool_use_id: "tu-preadmit-fresh-source".into(),
            delegation_call_id: source_task_id.into(),
        }),
    )
    .await
    .expect("source child");
    let runs = Arc::new(RunStore::new(db.clone()));
    let work_unit_key = "task|8|implementer|none";
    runs.insert_reserving(ReservingRunInsert {
        orchestration_binding: None,
        task_id: source_task_id.into(),
        root_task_id: source_task_id.into(),
        previous_task_id: None,
        generation: 1,
        parent_conversation_id: parent.id,
        parent_tool_use_id: Some("tu-preadmit-fresh-source".into()),
        child_conversation_id: old_child.id,
        agent_type: AgentType::Grok.to_string(),
        profile_id: None,
        workspace_path: Some(test_working_dir()),
        route_fingerprint: Some("aabbccdd".into()),
        launch_snapshot_version: Some("v1".into()),
        mode_id: Some("default".into()),
        config_values_json: Some("{}".into()),
        task_preview: Some(derive_task_preview("initial pre-admission dispatch")),
        request_fingerprint: Some(request_fingerprint(
            DELEGATE_TO_AGENT_TOOL,
            "initial pre-admission dispatch",
            Some(work_unit_key),
            None,
            None,
            None,
            "aabbccdd",
            None,
        )),
        admission_class: AdmissionClass::NormalRevision,
        lineage_root_task_id: source_task_id.into(),
        work_unit_key: Some(work_unit_key.into()),
        history_only: false,
        replaced_task_id: None,
        replacement_reason: None,
        started_at: Some(Utc::now()),
    })
    .await
    .expect("seed reserving source");

    let mock = Arc::new(MockSpawner::new());
    let broker = broker_with_run_store(mock.clone(), parent.id, runs.clone()).await;
    assert_eq!(
        broker
            .reconcile_running_on_startup()
            .await
            .expect("startup reconciliation"),
        1
    );
    let reconciled = runs
        .load_by_task_id(source_task_id)
        .await
        .expect("source lookup")
        .expect("source run");
    assert_eq!(reconciled.run_status, DelegationRunStatus::Failed);
    assert_eq!(reconciled.error_code.as_deref(), Some("host_restarted"));
    assert!(reconciled.reached_running_at.is_none());

    // Actual gen-1 broker admission: the identical key is legal without a
    // replacement because the interrupted source never reached running.
    mock.queue_spawn(Ok("pre-admission-fresh-redispatch".into()))
        .await;
    mock.queue_send(Ok(accepted(0, Utc::now()))).await;
    let redispatch = broker
        .start_delegation(delegate_req(
            parent.id,
            "tu-preadmit-fresh-redispatch",
            AgentType::Grok,
            "re-dispatch after startup reconciliation",
            "/tmp/codeg-preadmit-fresh",
            Some(work_unit_key),
        ))
        .await;
    assert_eq!(redispatch.status, TaskStatus::Running, "{redispatch:?}");
    let redispatch_task_id = redispatch.task_id.expect("redispatch task");
    let redispatch_run = runs
        .load_by_task_id(&redispatch_task_id)
        .await
        .expect("redispatch lookup")
        .expect("redispatch run");
    assert_eq!(
        redispatch_run.admission_class,
        AdmissionClass::NormalRevision
    );
    assert!(redispatch_run.previous_task_id.is_none());
    let redispatch_row = DelegationTaskRun::find_by_id(&redispatch_task_id)
        .one(&db.conn)
        .await
        .expect("redispatch raw row lookup")
        .expect("redispatch raw row");
    assert!(redispatch_row.replaced_task_id.is_none());
    assert_ne!(redispatch_run.child_conversation_id, old_child.id);
}

#[tokio::test]
async fn pre_admission_host_restarted_reserving_inherits_and_allows_redispatch() {
    let db = Arc::new(fresh_in_memory_db().await);
    let folder = seed_folder(&db, "/tmp/codeg-preadmit-continue").await;
    let parent = conversation_service::create(
        &db.conn,
        folder,
        AgentType::ClaudeCode,
        Some("preadmit continue parent".into()),
        None,
    )
    .await
    .expect("parent");
    let root_task_id = "pre-admission-restart-root";
    let child = conversation_service::create_with_delegation(
        &db.conn,
        folder,
        AgentType::Codex,
        Some("preadmit continue child".into()),
        None,
        Some(DelegationLink {
            parent_conversation_id: parent.id,
            parent_tool_use_id: "tu-preadmit-root".into(),
            delegation_call_id: root_task_id.into(),
        }),
    )
    .await
    .expect("child");
    set_child_external_id(&db, child.id, "session-preadmit-continue").await;

    let runs = Arc::new(RunStore::new(db.clone()));
    let mut reserving = unexpected_continue_insert(
        root_task_id,
        parent.id,
        child.id,
        1,
        None,
        root_task_id,
        "unit-preadmit-continue",
    );
    // Use the non-default class so the continued row proves inheritance rather
    // than merely taking the normal-revision default for completed rows.
    reserving.admission_class = AdmissionClass::UnexpectedContinue;
    runs.insert_reserving(reserving)
        .await
        .expect("durable reserving prior");

    assert_eq!(
        runs.reconcile_non_terminal(Utc::now())
            .await
            .expect("host restart reconciliation"),
        1
    );
    let restarted = runs
        .load_by_task_id(root_task_id)
        .await
        .expect("load restarted prior")
        .expect("restarted prior");
    assert_eq!(restarted.run_status, DelegationRunStatus::Failed);
    assert_eq!(restarted.error_code.as_deref(), Some("host_restarted"));
    assert_eq!(
        restarted.admission_class,
        AdmissionClass::UnexpectedContinue
    );
    assert!(restarted.reached_running_at.is_none());

    let mock = Arc::new(MockSpawner::new());
    let broker = broker_with_run_store(mock.clone(), parent.id, runs.clone()).await;
    mock.queue_spawn(Ok("pre-admission-resume".into())).await;
    mock.queue_send(Ok(accepted(child.id, Utc::now()))).await;
    let redispatch = broker
        .continue_delegation(continue_req(
            parent.id,
            "tu-preadmit-continue",
            root_task_id,
            "resume after pre-admission restart",
            Some("unit-preadmit-continue"),
        ))
        .await;

    assert_eq!(redispatch.status, TaskStatus::Running, "{redispatch:?}");
    assert_eq!(redispatch.reused_session, Some(true), "{redispatch:?}");
    assert_eq!(redispatch.child_conversation_id, Some(child.id));
    let continued_task_id = redispatch.task_id.expect("continued task id");
    let continued = runs
        .load_by_task_id(&continued_task_id)
        .await
        .expect("load continued row")
        .expect("continued row");
    assert_eq!(continued.previous_task_id.as_deref(), Some(root_task_id));
    assert_eq!(continued.generation, 2);
    assert_eq!(continued.run_status, DelegationRunStatus::Running);
    assert_eq!(
        continued.admission_class,
        AdmissionClass::UnexpectedContinue,
        "continue must inherit the pre-admission class"
    );
    assert!(continued.reached_running_at.is_some());
    let resume_args = mock.resume_args.lock().await;
    assert_eq!(resume_args.len(), 1, "actual resume path must be used");
    assert_eq!(
        resume_args[0].external_session_id,
        "session-preadmit-continue"
    );
}

#[tokio::test]
async fn pre_admission_replacement_retry_does_not_charge_until_running() {
    let db = Arc::new(fresh_in_memory_db().await);
    let folder = seed_folder(&db, "/tmp/codeg-preadmit-rep").await;
    let parent = conversation_service::create(
        &db.conn,
        folder,
        AgentType::ClaudeCode,
        Some("preadmit parent".into()),
        None,
    )
    .await
    .expect("parent");
    let runs = Arc::new(RunStore::new(db.clone()));
    let mock = Arc::new(MockSpawner::new());
    let broker = broker_with_run_store(mock.clone(), parent.id, runs.clone()).await;
    let unit = "unit-preadmit-rep";

    mock.queue_spawn(Ok("pa-root".into())).await;
    mock.queue_send(Ok(accepted(0, Utc::now()))).await;
    let root = broker
        .start_delegation(delegate_req(
            parent.id,
            "tu-pa-root",
            AgentType::Grok,
            "root",
            "/tmp/codeg-preadmit-rep",
            Some(unit),
        ))
        .await;
    let root_id = root.task_id.expect("root");
    let root_child = root.child_conversation_id.expect("child");
    {
        let row = DelegationTaskRun::find_by_id(&root_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut row = row.into_active_model();
        row.status = Set(DelegationRunStatus::Failed);
        row.error_code = Set(Some("unresumable".into()));
        row.termination_audit_json = Set(Some(unexpected_session_loss_audit(
            row.admission_class.clone().unwrap(),
        )));
        row.update(&db.conn).await.unwrap();
    }

    // First replacement attempt: fail spawn so promote_running never charges.
    mock.queue_spawn(Err(
        codeg_lib::acp::delegation::spawner::SpawnerError::Spawn("boom".into()),
    ))
    .await;
    let mut rep1 = delegate_req(
        parent.id,
        "tu-pa-rep1",
        AgentType::Grok,
        "replacement attempt 1",
        "/tmp/codeg-preadmit-rep",
        Some(unit),
    );
    rep1.replaces_task_id = Some(root_id.clone());
    rep1.replacement_reason = Some(REPLACEMENT_REASON_UNRESUMABLE.into());
    let r1 = broker.start_delegation(rep1).await;
    assert!(
        r1.status == TaskStatus::Failed || r1.error_code.is_some(),
        "{r1:?}"
    );

    let budget = delegation_lineage_budget::Entity::find()
        .all(&db.conn)
        .await
        .unwrap();
    for b in &budget {
        assert_eq!(
            b.replacement_count, 0,
            "pre-running failure must not charge replacement rail: {b:?}"
        );
    }
    let projection = runs
        .recovery_projection_for_task(&root_id)
        .await
        .expect("load retry projection")
        .expect("retry projection");
    assert_eq!(projection.disposition, "replace", "{projection:?}");
    assert_eq!(
        projection
            .replacement_reason
            .as_ref()
            .map(|reason| reason.as_str()),
        Some(REPLACEMENT_REASON_UNRESUMABLE),
        "{projection:?}"
    );

    // Retry with same replaces_task_id / reason / work_unit_key succeeds.
    mock.queue_spawn(Ok("pa-rep2".into())).await;
    mock.queue_send(Ok(accepted(0, Utc::now()))).await;
    let mut rep2 = delegate_req(
        parent.id,
        "tu-pa-rep2",
        AgentType::Grok,
        "replacement attempt 2",
        "/tmp/codeg-preadmit-rep",
        Some(unit),
    );
    rep2.replaces_task_id = Some(root_id.clone());
    rep2.replacement_reason = Some(REPLACEMENT_REASON_UNRESUMABLE.into());
    let r2 = broker.start_delegation(rep2).await;
    assert_eq!(r2.status, TaskStatus::Running, "{r2:?}");
    let _ = root_child;
}

// ---------------------------------------------------------------------------
// 9. Budget rails — no refund after running; cap
// ---------------------------------------------------------------------------

#[tokio::test]
async fn budget_no_refund_after_running_and_cap() {
    let db = Arc::new(fresh_in_memory_db().await);
    let folder = seed_folder(&db, "/tmp/codeg-budget-nr").await;
    let parent = conversation_service::create(
        &db.conn,
        folder,
        AgentType::ClaudeCode,
        Some("budget parent".into()),
        None,
    )
    .await
    .expect("parent");
    let runs = Arc::new(RunStore::new(db.clone()));
    let mock = Arc::new(MockSpawner::new());
    let broker = broker_with_run_store(mock.clone(), parent.id, runs.clone()).await;

    let (root_id, child_id) = start_and_complete(
        &broker,
        &mock,
        delegate_req(
            parent.id,
            "tu-bnr-root",
            AgentType::Codex,
            "root",
            "/tmp/codeg-budget-nr",
            Some("unit-bnr"),
        ),
        "bnr-root",
    )
    .await;
    set_child_external_id(&db, child_id, "sess-bnr").await;

    let mut target = root_id.clone();
    for i in 1..=UNEXPECTED_CONTINUE_LIMIT {
        // Mark latest as unexpected-canceled so continue admits UnexpectedContinue.
        let row = DelegationTaskRun::find_by_id(&target)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut row = row.into_active_model();
        row.status = Set(DelegationRunStatus::Canceled);
        row.error_code = Set(Some("host_restarted".into()));
        row.termination_audit_json = Set(Some(unexpected_host_restart_audit(
            row.admission_class.clone().unwrap(),
        )));
        row.update(&db.conn).await.unwrap();

        mock.queue_spawn(Ok(format!("bnr-uc-{i}"))).await;
        mock.queue_send(Ok(accepted(child_id, Utc::now()))).await;
        let cont = broker
            .continue_delegation(continue_req(
                parent.id,
                &format!("tu-bnr-uc-{i}"),
                &target,
                &format!("unexpected continue {i}"),
                Some("unit-bnr"),
            ))
            .await;
        assert_eq!(cont.status, TaskStatus::Running, "uc {i}: {cont:?}");
        let cont_id = cont.task_id.expect("id");
        // Settle terminal without refunding the charge already taken at promote.
        broker
            .complete_call(
                &cont_id,
                DelegationOutcome::from_err(
                    DelegationError::Canceled {
                        reason: "interrupted again".into(),
                    },
                    Some(child_id),
                ),
            )
            .await;
        // Model another host restart after this accepted continuation. The
        // broker's generic canceled outcome represents an explicit cancel,
        // which is not the scenario this budget fixture exercises.
        let settled = DelegationTaskRun::find_by_id(&cont_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut settled = settled.into_active_model();
        settled.status = Set(DelegationRunStatus::Canceled);
        settled.error_code = Set(Some("host_restarted".into()));
        settled.termination_audit_json = Set(Some(unexpected_host_restart_audit(
            settled.admission_class.clone().unwrap(),
        )));
        settled.update(&db.conn).await.unwrap();
        target = cont_id;
    }

    let lineage = runs
        .load_by_task_id(&root_id)
        .await
        .unwrap()
        .unwrap()
        .lineage_root_task_id;
    let budget = delegation_lineage_budget::Entity::find_by_id(&lineage)
        .one(&db.conn)
        .await
        .unwrap()
        .expect("lineage budget");
    assert_eq!(
        budget.unexpected_continue_count, UNEXPECTED_CONTINUE_LIMIT,
        "charges stick after terminal; no refund"
    );

    // A third unexpected continue is refused and projected as the required
    // same-key budget-exhausted replacement.
    let row = DelegationTaskRun::find_by_id(&target)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    let mut row = row.into_active_model();
    row.status = Set(DelegationRunStatus::Canceled);
    row.error_code = Set(Some("host_restarted".into()));
    row.termination_audit_json = Set(Some(unexpected_host_restart_audit(
        row.admission_class.clone().unwrap(),
    )));
    row.update(&db.conn).await.unwrap();

    let projection = runs
        .recovery_projection_for_task(&target)
        .await
        .expect("load exhausted projection")
        .expect("exhausted projection");
    assert_eq!(projection.disposition, "replace", "{projection:?}");
    assert_eq!(
        projection
            .replacement_reason
            .as_ref()
            .map(|reason| reason.as_str()),
        Some(REPLACEMENT_REASON_BUDGET_EXHAUSTED_CONTINUE),
        "{projection:?}"
    );

    let over = broker
        .continue_delegation(continue_req(
            parent.id,
            "tu-bnr-over",
            &target,
            "one too many",
            Some("unit-bnr"),
        ))
        .await;
    assert_eq!(
        over.error_code.as_deref(),
        Some("not_continuable"),
        "{over:?}"
    );
}

#[tokio::test]
async fn budget_race_allows_one_winner_for_final_unexpected_continue_slot() {
    use codeg_lib::acp::delegation::store::{
        PersistenceRetryPolicy, TaskStoreError, TerminalTaskWrite,
    };
    use codeg_lib::db::entities::conversation::ConversationStatus;
    use codeg_lib::db::entities::delegation_work_unit_budget;
    use codeg_lib::db::test_helpers::fresh_disk_db;
    use sea_orm::{ConnectOptions, ConnectionTrait, Database, DbBackend, Statement};

    let dir = tempfile::tempdir().expect("tempdir");
    let seed_db = Arc::new(fresh_disk_db(dir.path()).await);
    let folder = seed_folder(&seed_db, "/tmp/codeg-budget-race").await;
    let parent = conversation_service::create(
        &seed_db.conn,
        folder,
        AgentType::ClaudeCode,
        Some("budget race parent".into()),
        None,
    )
    .await
    .expect("parent");
    let child_a = conversation_service::create_with_delegation(
        &seed_db.conn,
        folder,
        AgentType::Codex,
        Some("budget race child a".into()),
        None,
        Some(DelegationLink {
            parent_conversation_id: parent.id,
            parent_tool_use_id: "tu-budget-race-a".into(),
            delegation_call_id: "budget-race-a".into(),
        }),
    )
    .await
    .expect("child a");
    let child_b = conversation_service::create_with_delegation(
        &seed_db.conn,
        folder,
        AgentType::Codex,
        Some("budget race child b".into()),
        None,
        Some(DelegationLink {
            parent_conversation_id: parent.id,
            parent_tool_use_id: "tu-budget-race-b".into(),
            delegation_call_id: "budget-race-b".into(),
        }),
    )
    .await
    .expect("child b");

    let lineage = "budget-race-lineage";
    let work_unit = "budget-race-work-unit";
    let seed_store = RunStore::new(seed_db.clone());
    seed_store
        .insert_reserving(unexpected_continue_insert(
            "budget-race-seed",
            parent.id,
            child_a.id,
            2,
            Some(lineage),
            lineage,
            work_unit,
        ))
        .await
        .expect("reserve first charged continuation");
    // Task 3/4: claim filter requires pre-bound child_connection_id.
    seed_store
        .bind_child_connection_while_reserving("budget-race-seed", "conn-budget-seed")
        .await
        .expect("bind seed before promote");
    seed_store
        .promote_running("budget-race-seed", "conn-budget-seed", Utc::now())
        .await
        .expect("charge first of two slots");
    seed_store
        .settle_terminal(
            "budget-race-seed",
            TerminalTaskWrite::completed(Utc::now(), ConversationStatus::Completed),
        )
        .await
        .expect("settle seed");
    seed_store
        .insert_reserving(unexpected_continue_insert(
            "budget-race-a",
            parent.id,
            child_a.id,
            3,
            Some("budget-race-seed"),
            lineage,
            work_unit,
        ))
        .await
        .expect("reserve racer a");
    seed_store
        .insert_reserving(unexpected_continue_insert(
            "budget-race-b",
            parent.id,
            child_b.id,
            2,
            Some("budget-race-seed"),
            lineage,
            work_unit,
        ))
        .await
        .expect("reserve racer b");

    drop(seed_store);
    let seed_db = Arc::try_unwrap(seed_db)
        .unwrap_or_else(|_| panic!("seed database must have no other Arc owners"));
    seed_db.conn.close().await.expect("close seed pool");

    async fn open_wal_pool(path: &std::path::Path) -> AppDatabase {
        let url = format!("sqlite:{}?mode=rwc", path.to_string_lossy());
        let mut options = ConnectOptions::new(url);
        options
            .max_connections(1)
            .min_connections(1)
            .connect_timeout(Duration::from_secs(10))
            .sqlx_logging(false);
        let conn = Database::connect(options).await.expect("open WAL pool");
        for pragma in [
            "PRAGMA journal_mode=WAL;",
            "PRAGMA busy_timeout=5000;",
            "PRAGMA foreign_keys=ON;",
        ] {
            conn.execute(Statement::from_string(DbBackend::Sqlite, pragma.to_owned()))
                .await
                .expect("set SQLite pragma");
        }
        AppDatabase { conn }
    }

    async fn promote_with_retry(
        store: &RunStore,
        task_id: &str,
        connection_id: &str,
    ) -> Result<(), TaskStoreError> {
        // Task 3/4: claim filter requires pre-bound child_connection_id.
        store
            .bind_child_connection_while_reserving(task_id, connection_id)
            .await?;
        let policy = PersistenceRetryPolicy::production();
        let mut attempt = 0;
        loop {
            match store
                .promote_running(task_id, connection_id, Utc::now())
                .await
            {
                Ok(_) => return Ok(()),
                Err(error) if error.is_budget_exhausted() => return Err(error),
                Err(error) if error.is_transient() && attempt + 1 < policy.max_attempts => {
                    tokio::time::sleep(policy.delay_for_attempt(attempt)).await;
                    attempt += 1;
                }
                Err(error) => return Err(error),
            }
        }
    }

    let path = dir.path().join("source.db");
    let pool_a = Arc::new(open_wal_pool(&path).await);
    let pool_b = Arc::new(open_wal_pool(&path).await);
    let store_a = Arc::new(RunStore::new(pool_a.clone()));
    let store_b = Arc::new(RunStore::new(pool_b.clone()));
    let barrier = Arc::new(Barrier::new(2));

    let (result_a, result_b) = tokio::join!(
        {
            let barrier = barrier.clone();
            let store = store_a.clone();
            async move {
                barrier.wait().await;
                promote_with_retry(&store, "budget-race-a", "conn-budget-a").await
            }
        },
        {
            let barrier = barrier.clone();
            let store = store_b.clone();
            async move {
                barrier.wait().await;
                promote_with_retry(&store, "budget-race-b", "conn-budget-b").await
            }
        },
    );

    let winners = [result_a.is_ok(), result_b.is_ok()]
        .into_iter()
        .filter(|won| *won)
        .count();
    let exhausted = [result_a.as_ref(), result_b.as_ref()]
        .into_iter()
        .filter(|result| matches!(result, Err(error) if error.is_budget_exhausted()))
        .count();
    assert_eq!(
        winners, 1,
        "one final budget slot: {result_a:?} / {result_b:?}"
    );
    assert_eq!(
        exhausted, 1,
        "the other promote must receive budget_exhausted: {result_a:?} / {result_b:?}"
    );

    let lineage_budget = delegation_lineage_budget::Entity::find_by_id(lineage)
        .one(&pool_a.conn)
        .await
        .unwrap()
        .expect("lineage budget");
    assert_eq!(
        lineage_budget.unexpected_continue_count, UNEXPECTED_CONTINUE_LIMIT,
        "the winner charges exactly the final slot"
    );
    let work_budget = delegation_work_unit_budget::Entity::find()
        .filter(delegation_work_unit_budget::Column::ParentConversationId.eq(parent.id))
        .filter(delegation_work_unit_budget::Column::WorkUnitKey.eq(work_unit))
        .one(&pool_a.conn)
        .await
        .unwrap()
        .expect("work-unit budget");
    assert_eq!(
        work_budget.unexpected_continue_count,
        UNEXPECTED_CONTINUE_LIMIT
    );

    let racer_a = store_a
        .load_by_task_id("budget-race-a")
        .await
        .unwrap()
        .expect("racer a");
    let racer_b = store_a
        .load_by_task_id("budget-race-b")
        .await
        .unwrap()
        .expect("racer b");
    let running = [&racer_a, &racer_b]
        .into_iter()
        .filter(|run| run.run_status == DelegationRunStatus::Running)
        .count();
    assert_eq!(running, 1, "only the charged runner may promote");
}

// ---------------------------------------------------------------------------
// 10. Desktop + web snapshot DTOs share the same core
// ---------------------------------------------------------------------------

#[tokio::test]
async fn desktop_and_web_snapshot_dto_share_core_and_immutability() {
    let db = Arc::new(fresh_in_memory_db().await);
    let folder = seed_folder(&db, "/tmp/codeg-snapshot-dto").await;
    let parent = conversation_service::create(
        &db.conn,
        folder,
        AgentType::ClaudeCode,
        Some("snapshot parent".into()),
        None,
    )
    .await
    .expect("parent");
    let runs = Arc::new(RunStore::new(db.clone()));
    let mock = Arc::new(MockSpawner::new());
    let broker = broker_with_run_store(mock.clone(), parent.id, runs.clone()).await;

    let (first_id, child_id) = start_and_complete(
        &broker,
        &mock,
        delegate_req(
            parent.id,
            "tu-snap-1",
            AgentType::Codex,
            "first run",
            "/tmp/codeg-snapshot-dto",
            None,
        ),
        "snap-1",
    )
    .await;
    set_child_external_id(&db, child_id, "sess-snap").await;
    let second_id = continue_and_complete(
        &broker,
        &mock,
        continue_req(parent.id, "tu-snap-2", &first_id, "second run", None),
        "snap-2",
        child_id,
        AgentType::Codex,
    )
    .await;

    // Advance child projection to the later run (simulates live latest).
    let mut child = conversation::Entity::find_by_id(child_id)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap()
        .into_active_model();
    child.delegation_call_id = Set(Some(second_id.clone()));
    child.delegation_run_generation = Set(Some(2));
    child.update(&db.conn).await.unwrap();

    // Desktop IPC delegates to this core helper.
    let desktop = get_delegation_run_snapshot_core(&db.conn, parent.id, &first_id)
        .await
        .expect("desktop/core snapshot");
    let data_dir = tempfile::tempdir().expect("web handler data dir");
    let web_state = Arc::new(AppState::new_for_test(
        AppDatabase {
            conn: db.conn.clone(),
        },
        data_dir.path().to_path_buf(),
    ));
    let web = codeg_lib::web::handlers::delegation::get_delegation_run_snapshot(
        axum::extract::Extension(web_state),
        axum::Json(
            codeg_lib::web::handlers::delegation::GetDelegationRunSnapshotParams {
                parent_conversation_id: parent.id,
                task_id: first_id.clone(),
            },
        ),
    )
    .await
    .expect("web snapshot handler")
    .0;
    assert_eq!(desktop, web, "desktop IPC and web HTTP share DTO identity");
    assert_eq!(desktop.task_id, first_id);
    assert_eq!(desktop.generation, 1);
    assert_eq!(desktop.child_conversation_id, child_id);
    assert_eq!(desktop.previous_task_id, None);

    let later = get_delegation_run_snapshot_core(&db.conn, parent.id, &second_id)
        .await
        .expect("later");
    assert_eq!(later.previous_task_id.as_deref(), Some(first_id.as_str()));
    assert_eq!(later.generation, 2);

    // Wrong parent fails closed.
    let err = get_delegation_run_snapshot_core(&db.conn, parent.id + 999, &first_id)
        .await
        .expect_err("foreign parent");
    assert_eq!(err.to_string(), "delegation run not found");
}

// Keep unused import noise down for BTreeMap if compiler complains.
#[allow(dead_code)]
fn _touch_btreemap() -> BTreeMap<String, String> {
    BTreeMap::new()
}

#[allow(dead_code)]
fn _touch_duration() -> Duration {
    Duration::from_millis(1)
}
