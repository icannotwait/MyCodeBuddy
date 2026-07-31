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
        .query_all(sql("SELECT name FROM sqlite_master \
             WHERE type='table' \
               AND name IN ( \
                 'delegation_workflows', \
                 'delegation_workflow_manifest_revisions', \
                 'delegation_workflow_node_bindings', \
                 'delegation_workflow_gate_settlements', \
                 'delegation_workflow_run_bindings' \
               ) \
             ORDER BY name"))
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

async fn table_columns(db: &DatabaseConnection, table: &str) -> Vec<(String, Option<String>)> {
    db.query_all(sql(format!("PRAGMA table_info({table})")))
        .await
        .unwrap()
        .into_iter()
        .map(|row| {
            (
                row.try_get::<String>("", "name").unwrap(),
                row.try_get::<Option<String>>("", "dflt_value").unwrap(),
            )
        })
        .collect()
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

    db.execute(sql("INSERT INTO delegation_workflows ( \
           workflow_id, parent_conversation_id, workflow_kind, schema_version, \
           active_manifest_revision, graph_revision, workflow_state, capability_version, \
           publication_token, supersedes_approved_revision, structural_revision, \
           created_at, updated_at \
         ) VALUES ( \
           'wf-1', 1, 'brainstorm_to_delivery', 1, \
           1, 1, 'estimated', 'workflow_manifest_v1', \
           'pub-token-1', NULL, 1, \
           '2026-07-26T00:00:00Z', '2026-07-26T00:00:00Z' \
         )"))
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

    db.execute(sql("INSERT INTO delegation_workflow_node_bindings ( \
           workflow_id, node_id, work_unit_key, role, agent_type, profile_id, phase_id, \
           task_index, introduced_revision, retired_revision, is_observed, retained_observed, \
           cohort_frozen, node_outcome, created_at, updated_at \
         ) VALUES ( \
           'wf-1', 'n1', 'task|1|implementer|grok|none', 'implementer', 'grok', NULL, 'tasks', \
           1, 1, NULL, 0, 0, \
           0, NULL, '2026-07-26T00:00:00Z', '2026-07-26T00:00:00Z' \
         )"))
        .await
        .expect("insert node binding");

    db.execute(sql("INSERT INTO delegation_workflow_gate_settlements ( \
           workflow_id, gate_id, gate_cycle, manifest_revision, outcome, \
           critical_count, important_count, minor_count, summary, \
           graph_revision_at_settle, created_at \
         ) VALUES ( \
           'wf-1', 'design_gate', 1, 1, 'approved', \
           0, 0, 0, 'ok', \
           1, '2026-07-26T00:00:00Z' \
         )"))
        .await
        .expect("insert gate settlement");

    db.execute(sql("INSERT INTO delegation_workflow_run_bindings ( \
           task_id, workflow_id, node_id, gate_id, gate_cycle, manifest_revision, \
           artifact_digest, reviewed_task_id, reviewed_implementer_generation, \
           lineage_ordinal, summary_validated, created_at, updated_at \
         ) VALUES ( \
           'task-1', 'wf-1', 'n1', NULL, NULL, 1, \
           NULL, NULL, NULL, \
           1, 0, '2026-07-26T00:00:00Z', '2026-07-26T00:00:00Z' \
         )"))
        .await
        .expect("insert run binding");

    assert_eq!(count_table(&db, "delegation_workflows").await, 1);
    assert_eq!(
        count_table(&db, "delegation_workflow_manifest_revisions").await,
        1
    );
    assert_eq!(
        count_table(&db, "delegation_workflow_node_bindings").await,
        1
    );
    assert_eq!(
        count_table(&db, "delegation_workflow_gate_settlements").await,
        1
    );
    assert_eq!(
        count_table(&db, "delegation_workflow_run_bindings").await,
        1
    );

    db.execute(sql(
        "DELETE FROM delegation_workflows WHERE workflow_id = 'wf-1'",
    ))
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
async fn manifest_v2_migration_preserves_freeze_and_adds_plan_evidence() {
    const MIGRATIONS_THROUGH_GATE_FINGERPRINTS: u32 = 43;

    let db = open_db().await;
    Migrator::up(&db, Some(MIGRATIONS_THROUGH_GATE_FINGERPRINTS))
        .await
        .unwrap();
    seed_folder_and_parent(&db).await;

    db.execute(sql("INSERT INTO delegation_workflows ( \
           workflow_id, parent_conversation_id, workflow_kind, schema_version, \
           active_manifest_revision, graph_revision, workflow_state, capability_version, \
           publication_token, created_at, updated_at \
         ) VALUES ( \
           'wf-v2-migration', 1, 'brainstorm_to_delivery', 1, \
           1, 1, 'estimated', 'workflow_manifest_v1', \
           'pub-v2-migration', '2026-07-27T00:00:00Z', '2026-07-27T00:00:00Z' \
         )"))
        .await
        .unwrap();
    db.execute(sql("INSERT INTO delegation_workflow_node_bindings ( \
           workflow_id, node_id, work_unit_key, role, agent_type, phase_id, \
           introduced_revision, is_observed, retained_observed, pair_frozen, \
           created_at, updated_at \
         ) VALUES ( \
           'wf-v2-migration', 'task-1-impl', 'task|1|implementer|codex|none', \
           'implementer', 'codex', 'tasks', 1, 0, 0, 1, \
           '2026-07-27T00:00:00Z', '2026-07-27T00:00:00Z' \
         )"))
        .await
        .unwrap();
    db.execute(sql("INSERT INTO delegation_workflow_gate_settlements ( \
           workflow_id, gate_id, gate_cycle, manifest_revision, structural_revision, \
           content_fingerprint, outcome, critical_count, important_count, minor_count, \
           summary, graph_revision_at_settle, created_at \
         ) VALUES ( \
           'wf-v2-migration', 'design_gate', 1, 1, 1, 'design-digest', 'approved', \
           0, 0, 0, 'approved', 1, '2026-07-27T00:00:00Z' \
         )"))
        .await
        .unwrap();

    Migrator::up(&db, None).await.unwrap();

    let binding_columns = table_columns(&db, "delegation_workflow_node_bindings").await;
    assert!(binding_columns
        .iter()
        .any(|(name, _)| name == "cohort_frozen"));
    assert!(!binding_columns
        .iter()
        .any(|(name, _)| name == "pair_frozen"));
    let row = db
        .query_one(sql(
            "SELECT cohort_frozen FROM delegation_workflow_node_bindings \
             WHERE workflow_id = 'wf-v2-migration' AND node_id = 'task-1-impl'",
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.try_get::<i64>("", "cohort_frozen").unwrap(), 1);

    let settlement_columns = table_columns(&db, "delegation_workflow_gate_settlements").await;
    let expected_columns = [
        ("review_scope", None),
        ("revision_kind", None),
        ("scope_reason", None),
        ("required_reviewer_node_ids_json", None),
        ("covered_author_task_id", None),
        ("covered_plan_digest", None),
        ("finding_ledger_json", None),
        ("net_improvement", None),
        ("stagnation_count", Some("0")),
        ("rewrite_used", Some("0")),
        ("next_action", None),
        ("report_files_json", None),
    ];
    for (name, default) in expected_columns {
        let actual = settlement_columns
            .iter()
            .find(|(column, _)| column == name)
            .unwrap_or_else(|| panic!("missing settlement column {name}"));
        assert_eq!(actual.1.as_deref(), default, "default for {name}");
    }

    let row =
        db.query_one(sql("SELECT review_scope, revision_kind, scope_reason, \
                    required_reviewer_node_ids_json, covered_author_task_id, \
                    covered_plan_digest, finding_ledger_json, net_improvement, \
                    stagnation_count, rewrite_used, next_action, report_files_json \
             FROM delegation_workflow_gate_settlements \
             WHERE workflow_id = 'wf-v2-migration' AND gate_id = 'design_gate'"))
            .await
            .unwrap()
            .unwrap();
    for name in [
        "review_scope",
        "revision_kind",
        "scope_reason",
        "required_reviewer_node_ids_json",
        "covered_author_task_id",
        "covered_plan_digest",
        "finding_ledger_json",
        "net_improvement",
        "next_action",
        "report_files_json",
    ] {
        assert_eq!(row.try_get::<Option<String>>("", name).unwrap(), None);
    }
    assert_eq!(row.try_get::<i64>("", "stagnation_count").unwrap(), 0);
    assert_eq!(row.try_get::<i64>("", "rewrite_used").unwrap(), 0);

    let invalid_check_values = [
        ("review_scope", "'partial'"),
        ("revision_kind", "'cosmetic'"),
        ("net_improvement", "2"),
        ("stagnation_count", "-1"),
        ("rewrite_used", "2"),
        ("next_action", "'retry_later'"),
    ];
    for (index, (column, invalid_value)) in invalid_check_values.into_iter().enumerate() {
        let update = db
            .execute(sql(format!(
                "UPDATE delegation_workflow_gate_settlements \
                 SET {column} = {invalid_value} \
                 WHERE workflow_id = 'wf-v2-migration' AND gate_id = 'design_gate'"
            )))
            .await;
        let update_error = update.expect_err("CHECK accepted invalid update");
        assert!(
            update_error.to_string().contains("CHECK constraint failed"),
            "invalid update for {column} failed for the wrong reason: {update_error}"
        );

        let insert = db
            .execute(sql(format!(
                "INSERT INTO delegation_workflow_gate_settlements ( \
                   workflow_id, gate_id, gate_cycle, manifest_revision, structural_revision, \
                   content_fingerprint, outcome, critical_count, important_count, minor_count, \
                   summary, graph_revision_at_settle, created_at, {column} \
                 ) VALUES ( \
                   'wf-v2-migration', 'invalid-{index}', 1, 1, 1, 'invalid-check', 'approved', \
                   0, 0, 0, 'invalid', 1, '2026-07-27T00:00:00Z', {invalid_value} \
                 )"
            )))
            .await;
        let insert_error = insert.expect_err("CHECK accepted invalid insert");
        assert!(
            insert_error.to_string().contains("CHECK constraint failed"),
            "invalid insert for {column} failed for the wrong reason: {insert_error}"
        );
    }

    let fresh = open_db().await;
    migrate_all(&fresh).await;
    assert_eq!(
        table_columns(&fresh, "delegation_workflow_node_bindings").await,
        binding_columns
    );
    assert_eq!(
        table_columns(&fresh, "delegation_workflow_gate_settlements").await,
        settlement_columns
    );
}

#[tokio::test]
async fn unique_parent_kind_and_publication_token() {
    let db = open_db().await;
    migrate_all(&db).await;
    seed_folder_and_parent(&db).await;

    db.execute(sql("INSERT INTO delegation_workflows ( \
           workflow_id, parent_conversation_id, workflow_kind, schema_version, \
           active_manifest_revision, graph_revision, workflow_state, capability_version, \
           publication_token, created_at, updated_at \
         ) VALUES ( \
           'wf-a', 1, 'brainstorm_to_delivery', 1, \
           1, 1, 'skeleton', 'workflow_manifest_v1', \
           'token-a', '2026-07-26T00:00:00Z', '2026-07-26T00:00:00Z' \
         )"))
        .await
        .expect("first workflow");

    let dup_kind = db
        .execute(sql("INSERT INTO delegation_workflows ( \
               workflow_id, parent_conversation_id, workflow_kind, schema_version, \
               active_manifest_revision, graph_revision, workflow_state, capability_version, \
               publication_token, created_at, updated_at \
             ) VALUES ( \
               'wf-b', 1, 'brainstorm_to_delivery', 1, \
               1, 1, 'skeleton', 'workflow_manifest_v1', \
               'token-b', '2026-07-26T00:00:00Z', '2026-07-26T00:00:00Z' \
             )"))
        .await;
    assert!(
        dup_kind.is_err(),
        "unique (parent_conversation_id, workflow_kind) must reject second header"
    );

    let dup_token = db
        .execute(sql("INSERT INTO delegation_workflows ( \
               workflow_id, parent_conversation_id, workflow_kind, schema_version, \
               active_manifest_revision, graph_revision, workflow_state, capability_version, \
               publication_token, created_at, updated_at \
             ) VALUES ( \
               'wf-c', 1, 'other_kind', 1, \
               1, 1, 'skeleton', 'workflow_manifest_v1', \
               'token-a', '2026-07-26T00:00:00Z', '2026-07-26T00:00:00Z' \
             )"))
        .await;
    assert!(
        dup_token.is_err(),
        "unique publication_token must reject second header with same token"
    );
}
