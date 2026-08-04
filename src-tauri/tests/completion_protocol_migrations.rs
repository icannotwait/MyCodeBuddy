use std::collections::BTreeMap;

use sea_orm::{
    ConnectionTrait, Database, DatabaseConnection, DbBackend, DbErr, QueryResult, Statement,
};
use sea_orm_migration::{MigrationTrait, MigratorTrait, SchemaManager};

use codeg_lib::db::migration::Migrator;

const MIGRATION_1: &str = "m20260804_000001_completion_protocol_and_run_evidence";
const MIGRATION_2: &str = "m20260804_000002_completion_scope_and_gate_settlement";
const PREVIOUS_MIGRATION: &str = "m20260731_000004_custom_agent_source";
const PRE_MANIFEST_V2_MIGRATION: &str = "m20260727_000002_workflow_gate_fingerprints";

const V1_SETTLEMENT_COLUMNS: &[&str] = &[
    "workflow_id",
    "gate_id",
    "gate_cycle",
    "manifest_revision",
    "structural_revision",
    "content_fingerprint",
    "outcome",
    "critical_count",
    "important_count",
    "minor_count",
    "summary",
    "graph_revision_at_settle",
    "review_scope",
    "revision_kind",
    "scope_reason",
    "required_reviewer_node_ids_json",
    "covered_author_task_id",
    "covered_plan_digest",
    "finding_ledger_json",
    "net_improvement",
    "stagnation_count",
    "rewrite_used",
    "next_action",
    "report_files_json",
    "lineage_reset_authorization_id",
    "created_at",
];

const V2_SETTLEMENT_COLUMNS: &[&str] = &[
    "evidence_scope_digest",
    "gate_lineage",
    "review_round",
    "required_node_set_json",
    "required_evidence_task_ids_json",
    "evidence_scope_digests_json",
    "localized_change_digest",
    "plan_round_state_v2_json",
];

const RUN_BINDING_SCOPE_COLUMNS: &[&str] = &[
    "evidence_scope_digest",
    "gate_lineage",
    "review_round",
    "instruction_block_digest",
    "material_selector_digest",
    "subject_material_digest",
    "requirements_identity",
    "task_specification_identity",
    "final_findings_identity",
    "producer_baseline_head",
];

const FAILPOINT_COPY: i64 = 0x434f_5001;
const FAILPOINT_SCHEMA: i64 = 0x5343_4801;
const FAILPOINT_INDEX: i64 = 0x494e_4401;
const FAILPOINT_FOREIGN_KEY_CHECK: i64 = 0x464b_4301;

fn sql(text: impl Into<String>) -> Statement {
    Statement::from_string(DbBackend::Sqlite, text.into())
}

struct BeforeCompletionProtocol;

#[async_trait::async_trait]
impl MigratorTrait for BeforeCompletionProtocol {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        let mut selected = Vec::new();
        for migration in Migrator::migrations() {
            if migration.name() == MIGRATION_1 {
                return selected;
            }
            selected.push(migration);
        }
        panic!("missing {MIGRATION_1}");
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TableColumn {
    cid: i64,
    name: String,
    data_type: String,
    not_null: bool,
    default_value: Option<String>,
    primary_key_ordinal: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ForeignKey {
    id: i64,
    sequence: i64,
    parent_table: String,
    from_column: String,
    to_column: Option<String>,
    on_update: String,
    on_delete: String,
    match_kind: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TableIndex {
    sequence: i64,
    name: String,
    unique: bool,
    origin: String,
    partial: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SettlementV1Projection {
    workflow_id: String,
    gate_id: String,
    gate_cycle: i64,
    manifest_revision: i64,
    structural_revision: i64,
    content_fingerprint: String,
    outcome: String,
    critical_count: Option<i64>,
    important_count: Option<i64>,
    minor_count: Option<i64>,
    summary: String,
    graph_revision_at_settle: i64,
    review_scope: Option<String>,
    revision_kind: Option<String>,
    scope_reason: Option<String>,
    required_reviewer_node_ids_json: Option<String>,
    covered_author_task_id: Option<String>,
    covered_plan_digest: Option<String>,
    finding_ledger_json: Option<String>,
    net_improvement: Option<i64>,
    stagnation_count: i64,
    rewrite_used: i64,
    next_action: Option<String>,
    report_files_json: Option<String>,
    lineage_reset_authorization_id: Option<String>,
    created_at: String,
}

async fn open_through(last_migration: &str) -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    let manager = SchemaManager::new(&db);
    let mut found = false;

    for migration in Migrator::migrations() {
        let name = migration.name();
        migration.up(&manager).await.unwrap();
        if name == last_migration {
            found = true;
            break;
        }
    }

    assert!(found, "missing migration {last_migration}");
    db
}

async fn apply_named(db: &DatabaseConnection, name: &str) -> Result<(), DbErr> {
    let manager = SchemaManager::new(db);
    for migration in Migrator::migrations() {
        if migration.name() == name {
            return migration.up(&manager).await;
        }
    }
    Err(DbErr::Custom(format!("missing migration {name}")))
}

async fn table_columns(db: &DatabaseConnection, table: &str) -> Vec<TableColumn> {
    db.query_all(sql(format!("PRAGMA table_info({table})")))
        .await
        .unwrap()
        .into_iter()
        .map(|row| TableColumn {
            cid: row.try_get("", "cid").unwrap(),
            name: row.try_get("", "name").unwrap(),
            data_type: row.try_get("", "type").unwrap(),
            not_null: row.try_get::<i64>("", "notnull").unwrap() != 0,
            default_value: row.try_get("", "dflt_value").unwrap(),
            primary_key_ordinal: row.try_get("", "pk").unwrap(),
        })
        .collect()
}

async fn table_foreign_keys(db: &DatabaseConnection, table: &str) -> Vec<ForeignKey> {
    db.query_all(sql(format!("PRAGMA foreign_key_list({table})")))
        .await
        .unwrap()
        .into_iter()
        .map(|row| ForeignKey {
            id: row.try_get("", "id").unwrap(),
            sequence: row.try_get("", "seq").unwrap(),
            parent_table: row.try_get("", "table").unwrap(),
            from_column: row.try_get("", "from").unwrap(),
            to_column: row.try_get("", "to").unwrap(),
            on_update: row.try_get("", "on_update").unwrap(),
            on_delete: row.try_get("", "on_delete").unwrap(),
            match_kind: row.try_get("", "match").unwrap(),
        })
        .collect()
}

async fn table_indexes(db: &DatabaseConnection, table: &str) -> Vec<TableIndex> {
    db.query_all(sql(format!("PRAGMA index_list({table})")))
        .await
        .unwrap()
        .into_iter()
        .map(|row| TableIndex {
            sequence: row.try_get("", "seq").unwrap(),
            name: row.try_get("", "name").unwrap(),
            unique: row.try_get::<i64>("", "unique").unwrap() != 0,
            origin: row.try_get("", "origin").unwrap(),
            partial: row.try_get::<i64>("", "partial").unwrap() != 0,
        })
        .collect()
}

async fn table_index_sql(db: &DatabaseConnection, table: &str) -> BTreeMap<String, Option<String>> {
    db.query_all(sql(format!(
        "SELECT name, sql FROM sqlite_master \
         WHERE type = 'index' AND tbl_name = '{table}' ORDER BY name"
    )))
    .await
    .unwrap()
    .into_iter()
    .map(|row| {
        (
            row.try_get::<String>("", "name").unwrap(),
            row.try_get::<Option<String>>("", "sql").unwrap(),
        )
    })
    .collect()
}

async fn table_sql(db: &DatabaseConnection, table: &str) -> String {
    db.query_one(sql(format!(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = '{table}'"
    )))
    .await
    .unwrap()
    .unwrap()
    .try_get("", "sql")
    .unwrap()
}

fn optional_column_expression(name: &str, columns: &[TableColumn]) -> String {
    if columns.iter().any(|column| column.name == name) {
        return name.to_owned();
    }
    match name {
        "stagnation_count" | "rewrite_used" => format!("0 AS {name}"),
        _ => format!("NULL AS {name}"),
    }
}

fn settlement_projection_from_row(row: &QueryResult) -> SettlementV1Projection {
    SettlementV1Projection {
        workflow_id: row.try_get("", "workflow_id").unwrap(),
        gate_id: row.try_get("", "gate_id").unwrap(),
        gate_cycle: row.try_get("", "gate_cycle").unwrap(),
        manifest_revision: row.try_get("", "manifest_revision").unwrap(),
        structural_revision: row.try_get("", "structural_revision").unwrap(),
        content_fingerprint: row.try_get("", "content_fingerprint").unwrap(),
        outcome: row.try_get("", "outcome").unwrap(),
        critical_count: row.try_get("", "critical_count").unwrap(),
        important_count: row.try_get("", "important_count").unwrap(),
        minor_count: row.try_get("", "minor_count").unwrap(),
        summary: row.try_get("", "summary").unwrap(),
        graph_revision_at_settle: row.try_get("", "graph_revision_at_settle").unwrap(),
        review_scope: row.try_get("", "review_scope").unwrap(),
        revision_kind: row.try_get("", "revision_kind").unwrap(),
        scope_reason: row.try_get("", "scope_reason").unwrap(),
        required_reviewer_node_ids_json: row
            .try_get("", "required_reviewer_node_ids_json")
            .unwrap(),
        covered_author_task_id: row.try_get("", "covered_author_task_id").unwrap(),
        covered_plan_digest: row.try_get("", "covered_plan_digest").unwrap(),
        finding_ledger_json: row.try_get("", "finding_ledger_json").unwrap(),
        net_improvement: row.try_get("", "net_improvement").unwrap(),
        stagnation_count: row.try_get("", "stagnation_count").unwrap(),
        rewrite_used: row.try_get("", "rewrite_used").unwrap(),
        next_action: row.try_get("", "next_action").unwrap(),
        report_files_json: row.try_get("", "report_files_json").unwrap(),
        lineage_reset_authorization_id: row.try_get("", "lineage_reset_authorization_id").unwrap(),
        created_at: row.try_get("", "created_at").unwrap(),
    }
}

async fn settlement_v1_projection(
    db: &DatabaseConnection,
    workflow_id: &str,
    gate_id: &str,
    gate_cycle: i64,
) -> SettlementV1Projection {
    let columns = table_columns(db, "delegation_workflow_gate_settlements").await;
    let select_list = V1_SETTLEMENT_COLUMNS
        .iter()
        .map(|name| optional_column_expression(name, &columns))
        .collect::<Vec<_>>()
        .join(", ");
    let row = db
        .query_one(sql(format!(
            "SELECT {select_list} FROM delegation_workflow_gate_settlements \
             WHERE workflow_id = '{workflow_id}' AND gate_id = '{gate_id}' \
               AND gate_cycle = {gate_cycle}"
        )))
        .await
        .unwrap()
        .unwrap();
    settlement_projection_from_row(&row)
}

async fn seed_v1_gate_settlement(
    db: &DatabaseConnection,
    workflow_id: &str,
    gate_id: &str,
    gate_cycle: i64,
    critical_count: i64,
    important_count: i64,
    minor_count: i64,
) {
    seed_legacy_workflow_and_run(
        db,
        workflow_id,
        &format!("task-{gate_cycle}"),
        "{\"kind\":\"author\"}",
    )
    .await;
    db.execute(sql(format!(
        "INSERT INTO delegation_workflow_gate_settlements (\
           workflow_id,gate_id,gate_cycle,manifest_revision,structural_revision,\
           content_fingerprint,outcome,critical_count,important_count,minor_count,\
           summary,graph_revision_at_settle,created_at\
         ) VALUES (\
           '{workflow_id}','{gate_id}',{gate_cycle},7,6,'sha256:content',\
           'changes_requested',{critical_count},{important_count},{minor_count},\
           'summary-v1',11,'2026-08-04T01:02:03Z'\
         )"
    )))
    .await
    .unwrap();

    let columns = table_columns(db, "delegation_workflow_gate_settlements").await;
    if columns.iter().any(|column| column.name == "review_scope") {
        db.execute(sql(format!(
            "UPDATE delegation_workflow_gate_settlements SET \
               review_scope = 'scoped', revision_kind = 'localized', \
               scope_reason = 'scope-v1', required_reviewer_node_ids_json = '[\"reviewer\"]', \
               covered_author_task_id = 'author-task', covered_plan_digest = 'sha256:plan', \
               finding_ledger_json = '{{\"minor\":5}}', net_improvement = 1, \
               stagnation_count = 4, rewrite_used = 1, next_action = 'continue_review', \
               report_files_json = '[\"report.md\"]' \
             WHERE workflow_id = '{workflow_id}' AND gate_id = '{gate_id}' \
               AND gate_cycle = {gate_cycle}"
        )))
        .await
        .unwrap();
    }
    if columns
        .iter()
        .any(|column| column.name == "lineage_reset_authorization_id")
    {
        db.execute(sql(format!(
            "UPDATE delegation_workflow_gate_settlements \
             SET lineage_reset_authorization_id = 'authorization-v1' \
             WHERE workflow_id = '{workflow_id}' AND gate_id = '{gate_id}' \
               AND gate_cycle = {gate_cycle}"
        )))
        .await
        .unwrap();
    }
}

async fn assert_foreign_key_check_clean(db: &DatabaseConnection) {
    assert!(
        db.query_all(sql("PRAGMA foreign_key_check"))
            .await
            .unwrap()
            .is_empty(),
        "foreign_key_check returned violations"
    );
}

async fn run_settlement_rebuild_with_failpoint(
    db: &DatabaseConnection,
    failpoint: &str,
) -> Result<(), DbErr> {
    let value = match failpoint {
        "copy" => FAILPOINT_COPY,
        "schema" => FAILPOINT_SCHEMA,
        "index" => FAILPOINT_INDEX,
        "foreign_key_check" => FAILPOINT_FOREIGN_KEY_CHECK,
        other => panic!("unknown failpoint {other}"),
    };
    db.execute(sql(format!("PRAGMA application_id = {value}")))
        .await?;
    let result = apply_named(db, MIGRATION_2).await;
    db.execute(sql("PRAGMA application_id = 0")).await?;
    result
}

fn column_map(columns: &[TableColumn]) -> BTreeMap<&str, &TableColumn> {
    columns
        .iter()
        .map(|column| (column.name.as_str(), column))
        .collect()
}

async fn assert_scope_state_schema(db: &DatabaseConnection) {
    let gate_state_columns = table_columns(db, "delegation_workflow_gate_states").await;
    assert_eq!(
        gate_state_columns
            .iter()
            .map(|column| (
                column.name.as_str(),
                column.data_type.as_str(),
                column.not_null
            ))
            .collect::<Vec<_>>(),
        vec![
            ("workflow_id", "TEXT", true),
            ("gate_id", "TEXT", true),
            ("gate_lineage", "TEXT", true),
            ("current_review_round", "INTEGER", true),
            ("selected_node_ids_json", "TEXT", true),
        ]
    );
    assert_eq!(gate_state_columns[0].primary_key_ordinal, 1);
    assert_eq!(gate_state_columns[1].primary_key_ordinal, 2);

    let design_root_columns = table_columns(db, "delegation_workflow_design_root_bindings").await;
    assert_eq!(
        design_root_columns
            .iter()
            .map(|column| column.name.as_str())
            .collect::<Vec<_>>(),
        vec![
            "workflow_id",
            "gate_id",
            "gate_lineage",
            "node_id",
            "task_id",
            "latest_run_id",
            "design_identity",
            "evidence_scope_digest",
            "graph_revision",
        ]
    );
    assert!(design_root_columns.iter().all(|column| column.not_null));
    assert_eq!(design_root_columns[0].primary_key_ordinal, 1);
    assert_eq!(design_root_columns[1].primary_key_ordinal, 2);
    assert_eq!(design_root_columns[2].primary_key_ordinal, 3);
    let design_fks = table_foreign_keys(db, "delegation_workflow_design_root_bindings").await;
    assert!(design_fks.iter().all(|foreign_key| {
        foreign_key.from_column != "task_id" && foreign_key.from_column != "latest_run_id"
    }));

    let package_columns = table_columns(db, "delegation_final_findings_packages").await;
    assert_eq!(
        package_columns
            .iter()
            .map(|column| column.name.as_str())
            .collect::<Vec<_>>(),
        vec![
            "package_id",
            "workflow_id",
            "gate_id",
            "gate_lineage",
            "source_evaluation_key",
            "source_evidence_task_ids_json",
            "items_json",
            "remediation_contexts_json",
            "package_digest",
            "status",
            "created_graph_revision",
            "resolved_graph_revision",
        ]
    );
    assert!(!column_map(&package_columns)["resolved_graph_revision"].not_null);
    let package_table_sql = table_sql(db, "delegation_final_findings_packages").await;
    for status in ["'active'", "'superseded'", "'resolved'"] {
        assert!(
            package_table_sql.contains(status),
            "missing status {status}"
        );
    }
    let package_index_sql = table_index_sql(db, "delegation_final_findings_packages").await;
    assert!(package_index_sql.values().any(|definition| {
        definition
            .as_deref()
            .is_some_and(|sql| sql.contains("WHERE status = 'active'"))
    }));

    let run_binding_columns = table_columns(db, "delegation_workflow_run_bindings").await;
    let run_binding_map = column_map(&run_binding_columns);
    for name in RUN_BINDING_SCOPE_COLUMNS {
        let column = run_binding_map
            .get(name)
            .unwrap_or_else(|| panic!("missing run-binding scope column {name}"));
        assert!(!column.not_null, "{name} must be nullable");
    }
}

async fn seed_legacy_workflow_and_run(
    db: &DatabaseConnection,
    workflow_id: &str,
    task_id: &str,
    card_summary_json: &str,
) {
    db.execute(sql("INSERT INTO folder \
         (id,name,path,last_opened_at,created_at,updated_at,is_open,sort_order,color,kind) \
         VALUES (1,'repo','C:/completion-protocol-fixture','2026-08-04','2026-08-04',\
                 '2026-08-04',1,1,'inherit','regular')"))
        .await
        .unwrap();
    db.execute(sql("INSERT INTO conversation \
         (id,folder_id,agent_type,status,kind,message_count,title_locked,auto_title_finalized, \
          parent_id,created_at,updated_at) VALUES \
         (1,1,'codex','completed','regular',0,0,0,NULL,\
          '2026-08-04T00:00:00Z','2026-08-04T00:00:00Z'),\
         (2,1,'codex','completed','delegate',0,0,0,1,\
          '2026-08-04T00:01:00Z','2026-08-04T00:02:00Z')"))
        .await
        .unwrap();
    db.execute(sql(format!(
        "INSERT INTO delegation_workflows (\
           workflow_id,parent_conversation_id,workflow_kind,schema_version,\
           active_manifest_revision,graph_revision,workflow_state,capability_version,\
           publication_token,structural_revision,design_fingerprint,plan_fingerprint,\
           created_at,updated_at\
         ) VALUES (\
           '{workflow_id}',1,'brainstorm_to_delivery',1,1,1,'approved',\
           'workflow_manifest_v1','publication-v1',1,'design-v1','plan-v1',\
           '2026-08-04T00:00:00Z','2026-08-04T00:00:00Z'\
         )"
    )))
    .await
    .unwrap();
    db.execute(sql(format!(
        "INSERT INTO delegation_task_runs (\
           task_id,root_task_id,generation,parent_conversation_id,child_conversation_id,\
           agent_type,admission_class,lineage_root_task_id,history_only,status,\
           card_summary_json,created_at,updated_at\
         ) VALUES (\
           '{task_id}','{task_id}',1,1,2,'codex','normal_revision','{task_id}',0,\
           'completed','{card_summary_json}','2026-08-04T00:01:00Z',\
           '2026-08-04T00:02:00Z'\
         )"
    )))
    .await
    .unwrap();
}

#[tokio::test]
async fn migration_1_labels_history_v1_without_touching_card_bytes() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    BeforeCompletionProtocol::up(&db, None).await.unwrap();
    seed_legacy_workflow_and_run(&db, "wf-v1", "task-v1", "{\"kind\":\"author\"}").await;

    Migrator::up(&db, None).await.unwrap();

    let row = db
        .query_one(sql(
            "SELECT completion_protocol_version, completion_protocol_mode \
             FROM delegation_workflows WHERE workflow_id = 'wf-v1'",
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.try_get::<i64>("", "completion_protocol_version")
            .unwrap(),
        1
    );
    assert_eq!(
        row.try_get::<String>("", "completion_protocol_mode")
            .unwrap(),
        "v1"
    );

    let run = db
        .query_one(sql(
            "SELECT card_summary_json, completion_state, completion_outcome, \
                    completion_evidence_json \
             FROM delegation_task_runs WHERE task_id = 'task-v1'",
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        run.try_get::<String>("", "card_summary_json").unwrap(),
        "{\"kind\":\"author\"}"
    );
    assert_eq!(
        run.try_get::<Option<String>>("", "completion_state")
            .unwrap(),
        None
    );
    assert_eq!(
        run.try_get::<Option<String>>("", "completion_outcome")
            .unwrap(),
        None
    );
    assert_eq!(
        run.try_get::<Option<String>>("", "completion_evidence_json")
            .unwrap(),
        None
    );
}

#[test]
fn migration_1_is_registered_before_the_other_completion_migrations() {
    let migrations = Migrator::migrations();
    let names: Vec<&str> = migrations
        .iter()
        .map(|migration| migration.name())
        .collect();
    let first = names.iter().position(|name| *name == MIGRATION_1).unwrap();
    let previous = first.checked_sub(1).and_then(|index| names.get(index));
    assert_eq!(previous.copied(), Some(PREVIOUS_MIGRATION));

    // Task 1 ships before migration 2; once appended, it must be the successor.
    if let Some(next) = names.get(first + 1) {
        assert_eq!(*next, MIGRATION_2);
    }
}

#[tokio::test]
async fn migration_2_preserves_v1_settlement_and_makes_only_counts_nullable() {
    let canonical_db = open_through(MIGRATION_1).await;
    let canonical_columns =
        table_columns(&canonical_db, "delegation_workflow_gate_settlements").await;
    let canonical_column_map = column_map(&canonical_columns);
    let canonical_foreign_keys =
        table_foreign_keys(&canonical_db, "delegation_workflow_gate_settlements").await;
    let canonical_indexes =
        table_indexes(&canonical_db, "delegation_workflow_gate_settlements").await;
    let canonical_index_sql =
        table_index_sql(&canonical_db, "delegation_workflow_gate_settlements").await;

    for (through, suffix) in [
        (PRE_MANIFEST_V2_MIGRATION, "pre-manifest-v2"),
        (MIGRATION_1, "current-v1"),
    ] {
        let db = open_through(through).await;
        let workflow_id = format!("wf-{suffix}");
        seed_v1_gate_settlement(&db, &workflow_id, "plan", 7, 2, 3, 5).await;

        let before_row = settlement_v1_projection(&db, &workflow_id, "plan", 7).await;
        let before_columns = table_columns(&db, "delegation_workflow_gate_settlements").await;
        let before_foreign_keys =
            table_foreign_keys(&db, "delegation_workflow_gate_settlements").await;
        let before_indexes = table_indexes(&db, "delegation_workflow_gate_settlements").await;
        let before_index_sql = table_index_sql(&db, "delegation_workflow_gate_settlements").await;
        let before_table_sql = table_sql(&db, "delegation_workflow_gate_settlements").await;

        apply_named(&db, MIGRATION_2).await.unwrap();

        assert_eq!(
            settlement_v1_projection(&db, &workflow_id, "plan", 7).await,
            before_row,
            "{suffix} row projection changed"
        );
        assert!(!before_table_sql.is_empty());

        let columns = table_columns(&db, "delegation_workflow_gate_settlements").await;
        let names = columns
            .iter()
            .map(|column| column.name.as_str())
            .collect::<Vec<_>>();
        let expected_names = V1_SETTLEMENT_COLUMNS
            .iter()
            .chain(V2_SETTLEMENT_COLUMNS)
            .copied()
            .collect::<Vec<_>>();
        assert_eq!(names, expected_names, "{suffix} column order changed");

        let columns_by_name = column_map(&columns);
        for name in V1_SETTLEMENT_COLUMNS {
            let actual = columns_by_name[name];
            let expected = canonical_column_map[name];
            assert_eq!(actual.data_type, expected.data_type, "{suffix} {name} type");
            assert_eq!(
                actual.default_value, expected.default_value,
                "{suffix} {name} default"
            );
            assert_eq!(
                actual.primary_key_ordinal, expected.primary_key_ordinal,
                "{suffix} {name} primary key"
            );
            let expected_not_null =
                !matches!(*name, "critical_count" | "important_count" | "minor_count")
                    && expected.not_null;
            assert_eq!(
                actual.not_null, expected_not_null,
                "{suffix} {name} nullability"
            );
        }
        for name in V2_SETTLEMENT_COLUMNS {
            assert!(!columns_by_name[name].not_null, "{name} must be nullable");
        }

        for before_column in before_columns {
            if !matches!(
                before_column.name.as_str(),
                "critical_count" | "important_count" | "minor_count"
            ) {
                let after = columns_by_name[before_column.name.as_str()];
                assert_eq!(after.data_type, before_column.data_type);
                assert_eq!(after.not_null, before_column.not_null);
                assert_eq!(after.default_value, before_column.default_value);
                assert_eq!(after.primary_key_ordinal, before_column.primary_key_ordinal);
            }
        }
        assert_eq!(
            table_foreign_keys(&db, "delegation_workflow_gate_settlements").await,
            canonical_foreign_keys,
            "{suffix} foreign keys changed"
        );
        assert_eq!(
            table_indexes(&db, "delegation_workflow_gate_settlements").await,
            canonical_indexes,
            "{suffix} indexes changed"
        );
        assert_eq!(
            table_index_sql(&db, "delegation_workflow_gate_settlements").await,
            canonical_index_sql,
            "{suffix} index SQL changed"
        );
        if through == MIGRATION_1 {
            assert_eq!(before_foreign_keys, canonical_foreign_keys);
            assert_eq!(before_indexes, canonical_indexes);
            assert_eq!(before_index_sql, canonical_index_sql);
        }

        assert_scope_state_schema(&db).await;
        assert_foreign_key_check_clean(&db).await;
    }
}

#[tokio::test]
async fn migration_2_rolls_back_copy_schema_index_and_fk_failures() {
    assert!(
        Migrator::migrations()
            .iter()
            .any(|migration| migration.name() == MIGRATION_2),
        "missing migration {MIGRATION_2}"
    );

    for failpoint in ["copy", "schema", "index", "foreign_key_check"] {
        let db = open_through(MIGRATION_1).await;
        seed_v1_gate_settlement(&db, "wf-rollback", "plan", 1, 1, 0, 0).await;
        let before_table_sql = table_sql(&db, "delegation_workflow_gate_settlements").await;
        let before_row = settlement_v1_projection(&db, "wf-rollback", "plan", 1).await;
        let before_columns = table_columns(&db, "delegation_workflow_gate_settlements").await;
        let before_foreign_keys =
            table_foreign_keys(&db, "delegation_workflow_gate_settlements").await;
        let before_indexes = table_indexes(&db, "delegation_workflow_gate_settlements").await;
        let before_index_sql = table_index_sql(&db, "delegation_workflow_gate_settlements").await;

        assert!(
            run_settlement_rebuild_with_failpoint(&db, failpoint)
                .await
                .is_err(),
            "{failpoint} did not fail"
        );
        assert_eq!(
            table_sql(&db, "delegation_workflow_gate_settlements").await,
            before_table_sql,
            "{failpoint} changed table SQL"
        );
        assert_eq!(
            settlement_v1_projection(&db, "wf-rollback", "plan", 1).await,
            before_row,
            "{failpoint} changed row bytes"
        );
        assert_eq!(
            table_columns(&db, "delegation_workflow_gate_settlements").await,
            before_columns,
            "{failpoint} changed table_info"
        );
        assert_eq!(
            table_foreign_keys(&db, "delegation_workflow_gate_settlements").await,
            before_foreign_keys,
            "{failpoint} changed foreign keys"
        );
        assert_eq!(
            table_indexes(&db, "delegation_workflow_gate_settlements").await,
            before_indexes,
            "{failpoint} changed indexes"
        );
        assert_eq!(
            table_index_sql(&db, "delegation_workflow_gate_settlements").await,
            before_index_sql,
            "{failpoint} changed index SQL"
        );
        assert!(
            table_columns(&db, "delegation_workflow_gate_settlements_v2")
                .await
                .is_empty(),
            "{failpoint} left the replacement table behind"
        );
        assert_foreign_key_check_clean(&db).await;
    }
}
