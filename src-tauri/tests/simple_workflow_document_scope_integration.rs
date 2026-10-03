//! Simple document history stays visible without assigning unrelated activity
//! to the current registered workflow.

#![cfg(feature = "test-utils")]

use chrono::Utc;
use codeg_lib::acp::delegation::transport::BrokerRegisterSimpleWorkflowRequest;
use codeg_lib::acp::delegation::workflow::dto::{ProjectedNodeStatus, WorkflowOverallState};
use codeg_lib::acp::delegation::workflow::project::project_workflow_graph_core;
use codeg_lib::acp::delegation::workflow::simple::{
    register_simple_workflow, register_simple_workflow_with_design,
};
use codeg_lib::db::entities::delegation_task_run::{self, AdmissionClass, DelegationRunStatus};
use codeg_lib::db::entities::simple_workflow;
use codeg_lib::db::migration::Migrator;
use codeg_lib::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
use codeg_lib::db::AppDatabase;
use codeg_lib::models::AgentType;
use sea_orm::{
    ActiveModelTrait, ConnectionTrait, Database, DbBackend, EntityTrait, Set, Statement,
};
use sea_orm_migration::MigratorTrait;

async fn completed_workflow() -> (AppDatabase, i32, tempfile::TempDir) {
    let db = fresh_in_memory_db().await;
    let workspace = tempfile::tempdir().expect("workspace");
    let folder = seed_folder(&db, workspace.path().to_str().unwrap()).await;
    let parent = seed_conversation(&db, folder, AgentType::Codex).await;
    std::fs::create_dir_all(workspace.path().join("docs")).unwrap();
    std::fs::write(
        workspace.path().join("docs/plan.md"),
        "## Task 1: Finish work\n",
    )
    .unwrap();
    let registration = register_simple_workflow(&db.conn, parent, "docs/plan.md", None)
        .await
        .unwrap();
    let progress = workspace
        .path()
        .join(registration.descriptor.progress_rel_path);
    std::fs::create_dir_all(progress.parent().unwrap()).unwrap();
    std::fs::write(
        progress,
        r#"<!-- codeg-simple-progress-v1
{"schema_version":1,"plan_rel_path":"docs/plan.md","tasks":[{"index":1,"status":"completed","commit":"abc123"}],"final_review_status":"completed"}
-->"#,
    )
    .unwrap();
    (db, parent, workspace)
}

async fn insert_document_run(db: &AppDatabase, parent: i32, task_id: &str, key: &str) {
    let now = Utc::now();
    let folder = seed_folder(db, &format!("/tmp/document-{task_id}")).await;
    let child = seed_conversation(db, folder, AgentType::Codex).await;
    delegation_task_run::ActiveModel {
        task_id: Set(task_id.into()),
        root_task_id: Set(task_id.into()),
        lineage_root_task_id: Set(task_id.into()),
        generation: Set(1),
        parent_conversation_id: Set(parent),
        child_conversation_id: Set(child),
        agent_type: Set("codex".into()),
        admission_class: Set(AdmissionClass::NormalRevision),
        reached_running_at: Set(Some(now)),
        work_unit_key: Set(Some(key.into())),
        history_only: Set(false),
        status: Set(DelegationRunStatus::Running),
        started_at: Set(Some(now)),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(&db.conn)
    .await
    .expect("insert document run");
}

#[tokio::test]
async fn unbound_design_history_does_not_reopen_completed_work() {
    let (db, parent, _workspace) = completed_workflow().await;
    insert_document_run(
        &db,
        parent,
        "unrelated-design",
        "design|docs/other-design.md|reviewer|codex|none",
    )
    .await;

    let snapshot = project_workflow_graph_core(&db, parent).await.unwrap();
    assert!(snapshot.nodes.iter().any(|node| {
        node.latest_task_id.as_deref() == Some("unrelated-design")
            && node.status == ProjectedNodeStatus::Running
    }));
    assert_eq!(snapshot.overall_state, WorkflowOverallState::Completed);
    assert!(snapshot.current_node_ids.is_empty());
    assert!(snapshot.current_phase_id.is_none());
}

#[tokio::test]
async fn active_current_plan_overrides_completed_tasks() {
    let (db, parent, _workspace) = completed_workflow().await;
    insert_document_run(
        &db,
        parent,
        "current-plan-review",
        "plan|docs/plan.md|reviewer|codex|none",
    )
    .await;
    let snapshot = project_workflow_graph_core(&db, parent).await.unwrap();
    let review = snapshot
        .nodes
        .iter()
        .find(|node| node.latest_task_id.as_deref() == Some("current-plan-review"))
        .unwrap();
    assert_eq!(snapshot.overall_state, WorkflowOverallState::InProgress);
    assert_eq!(snapshot.current_phase_id.as_deref(), Some("plan"));
    assert_eq!(snapshot.current_node_ids, vec![review.node_id.clone()]);
    assert_eq!(
        snapshot
            .nodes
            .iter()
            .find(|node| node.task_index == Some(1))
            .unwrap()
            .status,
        ProjectedNodeStatus::Completed
    );
}

#[tokio::test]
async fn reregistered_plan_excludes_previous_plan_history() {
    let (db, parent, workspace) = completed_workflow().await;
    insert_document_run(
        &db,
        parent,
        "old-plan-review",
        "plan|docs/plan.md|reviewer|codex|none",
    )
    .await;
    insert_document_run(
        &db,
        parent,
        "new-plan-author",
        "plan|docs/renamed.md|author|codex|none",
    )
    .await;
    let original = project_workflow_graph_core(&db, parent).await.unwrap();
    assert!(original
        .nodes
        .iter()
        .any(|node| node.latest_task_id.as_deref() == Some("old-plan-review")));
    assert!(original
        .nodes
        .iter()
        .all(|node| node.latest_task_id.as_deref() != Some("new-plan-author")));

    std::fs::write(
        workspace.path().join("docs/renamed.md"),
        "## Task 1: Renamed task\n",
    )
    .unwrap();
    register_simple_workflow(&db.conn, parent, "./docs//renamed.md", None)
        .await
        .unwrap();
    let renamed = project_workflow_graph_core(&db, parent).await.unwrap();
    assert!(renamed
        .nodes
        .iter()
        .all(|node| node.latest_task_id.as_deref() != Some("old-plan-review")));
    let author = renamed
        .nodes
        .iter()
        .find(|node| node.latest_task_id.as_deref() == Some("new-plan-author"))
        .unwrap();
    assert_eq!(renamed.current_node_ids, vec![author.node_id.clone()]);
    assert_eq!(renamed.current_phase_id.as_deref(), Some("plan"));
    assert_eq!(
        renamed.simple.as_ref().unwrap().plan_rel_path,
        "docs/renamed.md"
    );
}

#[tokio::test]
async fn invalid_plan_descriptor_cannot_admit_plan_runs_or_expose_locator() {
    let (db, parent, _workspace) = completed_workflow().await;
    insert_document_run(
        &db,
        parent,
        "safe-plan",
        "plan|docs/plan.md|reviewer|codex|none",
    )
    .await;
    insert_document_run(
        &db,
        parent,
        "unsafe-plan",
        "plan|C:/private/plan.md|reviewer|codex|none",
    )
    .await;
    let mut descriptor: simple_workflow::ActiveModel = simple_workflow::Entity::find_by_id(parent)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap()
        .into();
    descriptor.plan_rel_path = Set("C:/private/plan.md".into());
    descriptor.update(&db.conn).await.unwrap();
    let snapshot = project_workflow_graph_core(&db, parent).await.unwrap();
    assert!(snapshot.nodes.is_empty());
    assert!(snapshot.simple.is_none());
    assert!(snapshot.current_node_ids.is_empty());
    assert!(snapshot.current_phase_id.is_none());
    assert!(snapshot
        .projection_warning_codes
        .iter()
        .any(|code| code == "simple_plan_invalid_path"));
    assert!(!serde_json::to_string(&snapshot)
        .unwrap()
        .contains("C:/private"));
}

#[tokio::test]
async fn only_bound_design_reviewers_and_fixers_affect_current_activity() {
    for role in ["reviewer", "fixer"] {
        for status in [DelegationRunStatus::Running, DelegationRunStatus::Reserving] {
            let (db, parent, _workspace) = completed_workflow().await;
            register_simple_workflow_with_design(
                &db.conn,
                parent,
                "docs/plan.md",
                None,
                Some(Some("./docs//design.md")),
            )
            .await
            .unwrap();
            insert_document_run(
                &db,
                parent,
                "other-design",
                &format!("design|docs/other.md|{role}|codex|none"),
            )
            .await;
            let unrelated = project_workflow_graph_core(&db, parent).await.unwrap();
            assert_eq!(unrelated.overall_state, WorkflowOverallState::Completed);
            assert!(unrelated.current_node_ids.is_empty());

            insert_document_run(
                &db,
                parent,
                "bound-design",
                &format!("design|docs/design.md|{role}|codex|none"),
            )
            .await;
            let mut run: delegation_task_run::ActiveModel =
                delegation_task_run::Entity::find_by_id("bound-design")
                    .one(&db.conn)
                    .await
                    .unwrap()
                    .unwrap()
                    .into();
            run.status = Set(status);
            run.update(&db.conn).await.unwrap();
            let bound = project_workflow_graph_core(&db, parent).await.unwrap();
            let design = bound
                .nodes
                .iter()
                .find(|node| node.latest_task_id.as_deref() == Some("bound-design"))
                .unwrap();
            assert_eq!(bound.current_node_ids, vec![design.node_id.clone()]);
            assert_eq!(bound.current_phase_id.as_deref(), Some("design"));
            assert_eq!(bound.overall_state, WorkflowOverallState::InProgress);
            assert!(bound
                .nodes
                .iter()
                .any(|node| node.latest_task_id.as_deref() == Some("other-design")));

            register_simple_workflow_with_design(
                &db.conn,
                parent,
                "docs/plan.md",
                None,
                Some(None),
            )
            .await
            .unwrap();
            let cleared = project_workflow_graph_core(&db, parent).await.unwrap();
            assert_eq!(cleared.overall_state, WorkflowOverallState::Completed);
            assert!(cleared.current_node_ids.is_empty());
            assert_eq!(
                cleared.nodes, bound.nodes,
                "clearing identity preserves observed cards"
            );
        }
    }
}

#[tokio::test]
async fn design_binding_registration_preserves_replaces_clears_and_normalizes() {
    let (db, parent, _workspace) = completed_workflow().await;
    let bound = register_simple_workflow_with_design(
        &db.conn,
        parent,
        "docs/plan.md",
        None,
        Some(Some("./docs//design.md")),
    )
    .await
    .unwrap();
    assert!(bound.updated);
    assert_eq!(
        bound.descriptor.design_rel_path.as_deref(),
        Some("docs/design.md")
    );
    let replay = register_simple_workflow_with_design(
        &db.conn,
        parent,
        "./docs//plan.md",
        None,
        Some(Some("docs/design.md")),
    )
    .await
    .unwrap();
    assert!(!replay.updated);
    let preserved = register_simple_workflow(&db.conn, parent, "docs/plan.md", None)
        .await
        .unwrap();
    assert!(!preserved.updated);
    assert_eq!(
        preserved.descriptor.design_rel_path,
        bound.descriptor.design_rel_path
    );
    let progress_updated =
        register_simple_workflow(&db.conn, parent, "docs/plan.md", Some("state/progress.md"))
            .await
            .unwrap();
    assert!(progress_updated.updated);
    assert_eq!(
        progress_updated.descriptor.design_rel_path,
        bound.descriptor.design_rel_path
    );
    let replacement = register_simple_workflow_with_design(
        &db.conn,
        parent,
        "docs/plan.md",
        None,
        Some(Some("docs/revised-design.md")),
    )
    .await
    .unwrap();
    assert!(replacement.updated);
    assert_eq!(
        replacement.descriptor.design_rel_path.as_deref(),
        Some("docs/revised-design.md")
    );
    let cleared =
        register_simple_workflow_with_design(&db.conn, parent, "docs/plan.md", None, Some(None))
            .await
            .unwrap();
    assert!(cleared.updated);
    assert!(cleared.descriptor.design_rel_path.is_none());
    let clear_replay =
        register_simple_workflow_with_design(&db.conn, parent, "docs/plan.md", None, Some(None))
            .await
            .unwrap();
    assert!(!clear_replay.updated);
}

#[tokio::test]
async fn changing_plan_clears_old_design_unless_explicitly_rebound() {
    let (db, parent, workspace) = completed_workflow().await;
    register_simple_workflow_with_design(
        &db.conn,
        parent,
        "docs/plan.md",
        None,
        Some(Some("docs/design.md")),
    )
    .await
    .unwrap();
    insert_document_run(
        &db,
        parent,
        "old-design",
        "design|docs/design.md|reviewer|codex|none",
    )
    .await;
    std::fs::write(
        workspace.path().join("docs/new-plan.md"),
        "## Task 1: New work\n",
    )
    .unwrap();
    let changed = register_simple_workflow(&db.conn, parent, "docs/new-plan.md", None)
        .await
        .unwrap();
    assert!(changed.descriptor.design_rel_path.is_none());
    let unbound = project_workflow_graph_core(&db, parent).await.unwrap();
    assert_eq!(unbound.overall_state, WorkflowOverallState::Completed);
    assert!(unbound.current_node_ids.is_empty());
    let rebound = register_simple_workflow_with_design(
        &db.conn,
        parent,
        "docs/plan.md",
        None,
        Some(Some("docs/design.md")),
    )
    .await
    .unwrap();
    assert_eq!(
        rebound.descriptor.design_rel_path.as_deref(),
        Some("docs/design.md")
    );
    let active = project_workflow_graph_core(&db, parent).await.unwrap();
    assert_eq!(active.overall_state, WorkflowOverallState::InProgress);
    assert_eq!(active.current_phase_id.as_deref(), Some("design"));
}

#[tokio::test]
async fn invalid_design_updates_are_atomic_and_corrupt_binding_is_non_authoritative() {
    let (db, parent, _workspace) = completed_workflow().await;
    let valid = register_simple_workflow_with_design(
        &db.conn,
        parent,
        "docs/plan.md",
        None,
        Some(Some("docs/design.md")),
    )
    .await
    .unwrap();
    for path in [
        "",
        "../outside.md",
        "C:/private/design.md",
        "/private/design.md",
    ] {
        let error = register_simple_workflow_with_design(
            &db.conn,
            parent,
            "docs/new-plan.md",
            None,
            Some(Some(path)),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), "workflow_invalid_path");
        let persisted = simple_workflow::Entity::find_by_id(parent)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted, valid.descriptor);
    }
    let mut corrupt: simple_workflow::ActiveModel = valid.descriptor.into();
    corrupt.design_rel_path = Set(Some("C:/private/design.md".into()));
    corrupt.update(&db.conn).await.unwrap();
    insert_document_run(
        &db,
        parent,
        "unmatched-design",
        "design|docs/design.md|reviewer|codex|none",
    )
    .await;
    let snapshot = project_workflow_graph_core(&db, parent).await.unwrap();
    assert_eq!(snapshot.overall_state, WorkflowOverallState::Completed);
    assert!(snapshot.current_node_ids.is_empty());
    assert!(snapshot
        .projection_warning_codes
        .iter()
        .any(|code| code == "simple_design_invalid_path"));
    assert!(!serde_json::to_string(&snapshot)
        .unwrap()
        .contains("C:/private"));
}

#[tokio::test]
async fn bound_document_activity_preserves_blocked_final_review() {
    let (db, parent, workspace) = completed_workflow().await;
    let registration = register_simple_workflow_with_design(
        &db.conn,
        parent,
        "docs/plan.md",
        None,
        Some(Some("docs/design.md")),
    )
    .await
    .unwrap();
    let progress = workspace
        .path()
        .join(registration.descriptor.progress_rel_path);
    let text = std::fs::read_to_string(&progress).unwrap().replace(
        "\"final_review_status\":\"completed\"",
        "\"final_review_status\":\"blocked\"",
    );
    std::fs::write(progress, text).unwrap();
    insert_document_run(
        &db,
        parent,
        "bound-design",
        "design|docs/design.md|reviewer|codex|none",
    )
    .await;
    let snapshot = project_workflow_graph_core(&db, parent).await.unwrap();
    assert_eq!(snapshot.overall_state, WorkflowOverallState::Blocked);
    assert_eq!(snapshot.current_phase_id.as_deref(), Some("design"));
}

#[tokio::test]
async fn finished_latest_bound_design_does_not_reopen_completed_work() {
    let (db, parent, _workspace) = completed_workflow().await;
    register_simple_workflow_with_design(
        &db.conn,
        parent,
        "docs/plan.md",
        None,
        Some(Some("docs/design.md")),
    )
    .await
    .unwrap();
    for task_id in ["earlier-completed", "latest-completed"] {
        insert_document_run(
            &db,
            parent,
            task_id,
            "design|docs/design.md|reviewer|codex|none",
        )
        .await;
        if task_id == "earlier-completed" {
            let mut earlier: delegation_task_run::ActiveModel =
                delegation_task_run::Entity::find_by_id(task_id)
                    .one(&db.conn)
                    .await
                    .unwrap()
                    .unwrap()
                    .into();
            earlier.status = Set(DelegationRunStatus::Completed);
            earlier.finished_at = Set(Some(Utc::now()));
            earlier.update(&db.conn).await.unwrap();
        }
    }
    let mut latest: delegation_task_run::ActiveModel =
        delegation_task_run::Entity::find_by_id("latest-completed")
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap()
            .into();
    latest.generation = Set(2);
    let latest = latest.update(&db.conn).await.unwrap();
    assert_eq!(
        project_workflow_graph_core(&db, parent)
            .await
            .unwrap()
            .overall_state,
        WorkflowOverallState::InProgress
    );
    let mut latest: delegation_task_run::ActiveModel = latest.into();
    latest.status = Set(DelegationRunStatus::Completed);
    latest.finished_at = Set(Some(Utc::now()));
    latest.update(&db.conn).await.unwrap();
    let snapshot = project_workflow_graph_core(&db, parent).await.unwrap();
    let design = snapshot
        .nodes
        .iter()
        .find(|node| node.latest_task_id.as_deref() == Some("latest-completed"))
        .unwrap();
    assert_eq!(design.run_count, 2);
    assert_eq!(design.status, ProjectedNodeStatus::Completed);
    assert_eq!(snapshot.overall_state, WorkflowOverallState::Completed);
    assert!(snapshot.current_node_ids.is_empty());
}

#[tokio::test]
async fn design_binding_migration_keeps_legacy_rows_unbound_and_is_reversible() {
    let conn = Database::connect("sqlite::memory:").await.unwrap();
    let preceding = Migrator::migrations()
        .iter()
        .position(|migration| migration.name() == "m20261003_000001_simple_workflow_design_binding")
        .expect("Design binding migration is registered") as u32;
    Migrator::up(&conn, Some(preceding)).await.unwrap();
    let db = AppDatabase { conn };
    let folder = seed_folder(&db, "/tmp/legacy-design-binding").await;
    let parent = seed_conversation(&db, folder, AgentType::Codex).await;
    db.conn.execute(Statement::from_sql_and_values(DbBackend::Sqlite,
        "INSERT INTO simple_workflows (parent_conversation_id, plan_rel_path, progress_rel_path, created_at, updated_at) VALUES (?, 'docs/plan.md', 'progress.md', '2026-10-03T00:00:00Z', '2026-10-03T00:00:00Z')",
        [parent.into()])).await.unwrap();
    Migrator::up(&db.conn, Some(1)).await.unwrap();
    let legacy = simple_workflow::Entity::find_by_id(parent)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
    assert!(legacy.design_rel_path.is_none());
    assert_eq!(legacy.plan_rel_path, "docs/plan.md");
    register_simple_workflow_with_design(
        &db.conn,
        parent,
        "docs/plan.md",
        Some("progress.md"),
        Some(Some("docs/design.md")),
    )
    .await
    .unwrap();
    Migrator::down(&db.conn, Some(1)).await.unwrap();
    let row = db
        .conn
        .query_one(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT plan_rel_path FROM simple_workflows".to_string(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.try_get::<String>("", "plan_rel_path").unwrap(),
        "docs/plan.md"
    );
    Migrator::up(&db.conn, Some(1)).await.unwrap();
    assert!(simple_workflow::Entity::find_by_id(parent)
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap()
        .design_rel_path
        .is_none());
}

#[test]
fn design_binding_wire_distinguishes_omission_null_and_path() {
    for (field, expected) in [
        (None, None),
        (Some(serde_json::Value::Null), Some(None)),
        (
            Some(serde_json::json!("docs/design.md")),
            Some(Some("docs/design.md".to_string())),
        ),
    ] {
        let mut json = serde_json::json!({"token":"test", "plan_rel_path":"docs/plan.md"});
        if let Some(field) = field {
            json["design_rel_path"] = field;
        }
        let request: BrokerRegisterSimpleWorkflowRequest =
            serde_json::from_value(json.clone()).unwrap();
        assert_eq!(request.design_rel_path, expected);
        assert_eq!(serde_json::to_value(request).unwrap(), json);
    }
    assert!(
        serde_json::from_value::<BrokerRegisterSimpleWorkflowRequest>(serde_json::json!({
            "token":"test", "plan_rel_path":"docs/plan.md", "design_rel_path": 42
        }))
        .is_err()
    );
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../src/acp/delegation/tool_schema.json")).unwrap();
    let registration = schema
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "register_simple_workflow")
        .unwrap();
    assert_eq!(
        registration["inputSchema"]["properties"]["design_rel_path"]["type"],
        serde_json::json!(["string", "null"])
    );
    assert_eq!(
        registration["inputSchema"]["required"],
        serde_json::json!(["plan_rel_path"])
    );
}
