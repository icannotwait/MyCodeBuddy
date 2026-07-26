//! Migration tests for delegation workflow graph tables (Task 1).
//!
//! Applies full Migrator, asserts five workflow tables + unique indexes,
//! and verifies ON DELETE CASCADE from header → child rows.

use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement};
use sea_orm_migration::MigratorTrait;

use codeg_lib::db::migration::Migrator;

fn sql(text: impl Into<String>) -> Statement {
    Statement::from_string(DbBackend::Sqlite, text.into())
}

async fn open_db() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    db.execute(sql("PRAGMA foreign_keys=ON;")).await.unwrap();
    db
}

async fn migrate_all(db: &DatabaseConnection) {
    Migrator::up(db, None).await.unwrap();
}

async fn table_names(db: &DatabaseConnection) -> Vec<String> {
    let rows = db
        .query_all(sql(
            "SELECT name FROM sqlite_master \
             WHERE type='table' \
               AND name IN ( \
                 'delegation_workflows', \
                 'delegation_workflow_manifest_revisions', \
                 'delegation_workflow_node_bindings', \
                 'delegation_workflow_gate_settlements', \
                 'delegation_workflow_run_bindings' \
               ) \
             ORDER BY name",
        ))
        .await
        .unwrap();
    rows.into_iter()
        .map(|r| r.try_get::<String>("", "name").unwrap())
        .collect()
}

async fn index_exists(db: &DatabaseConnection, name: &str) -> bool {
    let row = db
        .query_one(sql(format!(
            "SELECT name FROM sqlite_master WHERE type='index' AND name='{name}'"
        )))
        .await
        .unwrap();
    row.is_some()
}

async fn seed_folder_and_parent(db: &DatabaseConnection) {
    db.execute(sql(
        "INSERT INTO folder \
         (id,name,path,last_opened_at,created_at,updated_at,is_open,sort_order,color,kind) \
         VALUES (1,'repo','/tmp/wf-mig','2026-07-01','2026-07-01','2026-07-01',1,1,'inherit','regular')",
    ))
    .await
    .expect("seed folder");
    db.execute(sql(
        "INSERT INTO conversation \
         (id,folder_id,agent_type,status,kind,message_count,title_locked,auto_title_finalized, \
          created_at,updated_at) \
         VALUES (1,1,'codex','completed','regular',0,0,0,'2026-07-01','2026-07-01')",
    ))
    .await
    .expect("seed parent conversation");
}

async fn count_table(db: &DatabaseConnection, table: &str) -> i64 {
    let row = db
        .query_one(sql(format!("SELECT COUNT(*) AS c FROM {table}")))
        .await
        .unwrap()
        .unwrap();
    row.try_get::<i64>("", "c").unwrap()
}

#[tokio::test]
async fn workflow_tables_exist_after_migration() {
    let db = open_db().await;
    migrate_all(&db).await;

    let names = table_names(&db).await;
    assert_eq!(
        names,
        vec![
            "delegation_workflow_gate_settlements".to_string(),
            "delegation_workflow_manifest_revisions".to_string(),
            "delegation_workflow_node_bindings".to_string(),
            "delegation_workflow_run_bindings".to_string(),
            "delegation_workflows".to_string(),
        ]
    );

    for idx in [
        "idx_dw_parent_kind",
        "idx_dw_publication_token",
        "idx_dwgs_gate_cycle",
        "idx_dwnb_workflow_node",
        "idx_dwnb_workflow_key",
        "idx_dwrb_task",
    ] {
        assert!(
            index_exists(&db, idx).await,
            "expected unique index {idx} to exist"
        );
    }
}

#[tokio::test]
async fn delete_workflow_cascades_to_child_tables() {
    let db = open_db().await;
    migrate_all(&db).await;
    seed_folder_and_parent(&db).await;

    db.execute(sql(
        "INSERT INTO delegation_workflows ( \
           workflow_id, parent_conversation_id, workflow_kind, schema_version, \
           active_manifest_revision, graph_revision, workflow_state, capability_version, \
           publication_token, supersedes_approved_revision, created_at, updated_at \
         ) VALUES ( \
           'wf-1', 1, 'brainstorm_to_delivery', 1, \
           1, 1, 'estimated', 'workflow_manifest_v1', \
           'pub-token-1', NULL, '2026-07-26T00:00:00Z', '2026-07-26T00:00:00Z' \
         )",
    ))
    .await
    .expect("insert workflow header");

    db.execute(sql(
        "INSERT INTO delegation_workflow_manifest_revisions ( \
           workflow_id, manifest_revision, manifest_state, document_json, document_digest, created_at \
         ) VALUES ( \
           'wf-1', 1, 'estimated', '{}', 'digest-1', '2026-07-26T00:00:00Z' \
         )",
    ))
    .await
    .expect("insert manifest revision");

    db.execute(sql(
        "INSERT INTO delegation_workflow_node_bindings ( \
           workflow_id, node_id, work_unit_key, role, agent_type, profile_id, phase_id, \
           task_index, introduced_revision, retired_revision, is_observed, retained_observed, \
           pair_frozen, node_outcome, created_at, updated_at \
         ) VALUES ( \
           'wf-1', 'n1', 'task|1|implementer|grok|none', 'implementer', 'grok', NULL, 'tasks', \
           1, 1, NULL, 0, 0, \
           0, NULL, '2026-07-26T00:00:00Z', '2026-07-26T00:00:00Z' \
         )",
    ))
    .await
    .expect("insert node binding");

    db.execute(sql(
        "INSERT INTO delegation_workflow_gate_settlements ( \
           workflow_id, gate_id, gate_cycle, manifest_revision, outcome, \
           critical_count, important_count, minor_count, summary, \
           graph_revision_at_settle, created_at \
         ) VALUES ( \
           'wf-1', 'design_gate', 1, 1, 'approved', \
           0, 0, 0, 'ok', \
           1, '2026-07-26T00:00:00Z' \
         )",
    ))
    .await
    .expect("insert gate settlement");

    db.execute(sql(
        "INSERT INTO delegation_workflow_run_bindings ( \
           task_id, workflow_id, node_id, gate_id, gate_cycle, manifest_revision, \
           artifact_digest, reviewed_task_id, reviewed_implementer_generation, \
           lineage_ordinal, summary_validated, created_at, updated_at \
         ) VALUES ( \
           'task-1', 'wf-1', 'n1', NULL, NULL, 1, \
           NULL, NULL, NULL, \
           1, 0, '2026-07-26T00:00:00Z', '2026-07-26T00:00:00Z' \
         )",
    ))
    .await
    .expect("insert run binding");

    assert_eq!(count_table(&db, "delegation_workflows").await, 1);
    assert_eq!(
        count_table(&db, "delegation_workflow_manifest_revisions").await,
        1
    );
    assert_eq!(count_table(&db, "delegation_workflow_node_bindings").await, 1);
    assert_eq!(
        count_table(&db, "delegation_workflow_gate_settlements").await,
        1
    );
    assert_eq!(count_table(&db, "delegation_workflow_run_bindings").await, 1);

    db.execute(sql("DELETE FROM delegation_workflows WHERE workflow_id = 'wf-1'"))
        .await
        .expect("delete workflow header");

    assert_eq!(count_table(&db, "delegation_workflows").await, 0);
    assert_eq!(
        count_table(&db, "delegation_workflow_manifest_revisions").await,
        0,
        "manifest revisions must cascade-delete with workflow"
    );
    assert_eq!(
        count_table(&db, "delegation_workflow_node_bindings").await,
        0,
        "node bindings must cascade-delete with workflow"
    );
    assert_eq!(
        count_table(&db, "delegation_workflow_gate_settlements").await,
        0,
        "gate settlements must cascade-delete with workflow"
    );
    assert_eq!(
        count_table(&db, "delegation_workflow_run_bindings").await,
        0,
        "run bindings must cascade-delete with workflow"
    );
}

#[tokio::test]
async fn unique_parent_kind_and_publication_token() {
    let db = open_db().await;
    migrate_all(&db).await;
    seed_folder_and_parent(&db).await;

    db.execute(sql(
        "INSERT INTO delegation_workflows ( \
           workflow_id, parent_conversation_id, workflow_kind, schema_version, \
           active_manifest_revision, graph_revision, workflow_state, capability_version, \
           publication_token, created_at, updated_at \
         ) VALUES ( \
           'wf-a', 1, 'brainstorm_to_delivery', 1, \
           1, 1, 'skeleton', 'workflow_manifest_v1', \
           'token-a', '2026-07-26T00:00:00Z', '2026-07-26T00:00:00Z' \
         )",
    ))
    .await
    .expect("first workflow");

    let dup_kind = db
        .execute(sql(
            "INSERT INTO delegation_workflows ( \
               workflow_id, parent_conversation_id, workflow_kind, schema_version, \
               active_manifest_revision, graph_revision, workflow_state, capability_version, \
               publication_token, created_at, updated_at \
             ) VALUES ( \
               'wf-b', 1, 'brainstorm_to_delivery', 1, \
               1, 1, 'skeleton', 'workflow_manifest_v1', \
               'token-b', '2026-07-26T00:00:00Z', '2026-07-26T00:00:00Z' \
             )",
        ))
        .await;
    assert!(
        dup_kind.is_err(),
        "unique (parent_conversation_id, workflow_kind) must reject second header"
    );

    let dup_token = db
        .execute(sql(
            "INSERT INTO delegation_workflows ( \
               workflow_id, parent_conversation_id, workflow_kind, schema_version, \
               active_manifest_revision, graph_revision, workflow_state, capability_version, \
               publication_token, created_at, updated_at \
             ) VALUES ( \
               'wf-c', 1, 'other_kind', 1, \
               1, 1, 'skeleton', 'workflow_manifest_v1', \
               'token-a', '2026-07-26T00:00:00Z', '2026-07-26T00:00:00Z' \
             )",
        ))
        .await;
    assert!(
        dup_token.is_err(),
        "unique publication_token must reject second header with same token"
    );
}
