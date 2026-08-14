use std::collections::BTreeMap;

use sea_orm::{
    ConnectionTrait, Database, DatabaseConnection, DbBackend, DbErr, QueryResult, Statement,
};
use sea_orm_migration::{MigrationTrait, MigratorTrait, SchemaManager};

use codeg_lib::db::migration::Migrator;

const MIGRATION_1: &str = "m20260804_000001_completion_protocol_and_run_evidence";
const MIGRATION_2: &str = "m20260804_000002_completion_scope_and_gate_settlement";
const MIGRATION_3: &str = "m20260804_000003_completion_tool_intents_and_restart_link";
const MIGRATION_4: &str = "m20260804_000004_typed_completion_attention";
const MIGRATION_V2_ONLY: &str = "m20260809_000001_completion_protocol_v2_only";
const PRE_V2_ONLY_MIGRATION: &str = "m20260806_000004_legacy_restart_context";
// Immediate predecessor in the merged (chronological) chain. Fork work-task /
// token-usage migrations sit between custom_agent_source and completion protocol 1.
const PREVIOUS_MIGRATION: &str = "m20260803_000001_token_usage";
const PRE_MANIFEST_V2_MIGRATION: &str = "m20260727_000002_workflow_gate_fingerprints";
const FORK_MIGRATIONS_BEFORE_V2_ONLY: &[&str] = &[
    "m20260807_000001_work_task_scheduled_at",
    "m20260808_000001_custom_agent_supports_mcp",
];

const V2_ONLY_TRIGGERS: &[&str] = &[
    "trg_delegation_workflows_legacy_source_frozen",
    "trg_delegation_workflows_protocol_frozen",
    "trg_delegation_workflows_v2_only_insert",
];

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

struct BeforeCompletionProtocolV2Only;

#[async_trait::async_trait]
impl MigratorTrait for BeforeCompletionProtocolV2Only {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        let mut selected = Vec::new();
        for migration in Migrator::migrations() {
            let is_predecessor = migration.name() == PRE_V2_ONLY_MIGRATION;
            selected.push(migration);
            if is_predecessor {
                return selected;
            }
        }
        panic!("missing {PRE_V2_ONLY_MIGRATION}");
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct ChildQuestionProjection {
    request_id: String,
    task_id: String,
    parent_conversation_id: i32,
    child_conversation_id: i32,
    child_tool_call_id: String,
    status: String,
    message: String,
    reply: Option<String>,
    resolution_code: Option<String>,
    created_at: String,
    resolved_at: Option<String>,
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

async fn table_exists(db: &DatabaseConnection, table: &str) -> bool {
    db.query_one(sql(format!(
        "SELECT 1 AS present FROM sqlite_master WHERE type = 'table' AND name = '{table}'"
    )))
    .await
    .unwrap()
    .is_some()
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

async fn run_attention_rebuild_with_failpoint(
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
    let result = apply_named(db, MIGRATION_4).await;
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

async fn seed_workflow(
    db: &DatabaseConnection,
    workflow_id: &str,
    completion_protocol_version: i64,
    completion_protocol_mode: &str,
) {
    db.execute(sql("INSERT OR IGNORE INTO folder \
         (id,name,path,last_opened_at,created_at,updated_at,is_open,sort_order,color,kind) \
         VALUES (1,'repo','C:/completion-protocol-fixture','2026-08-04','2026-08-04',\
                 '2026-08-04',1,1,'inherit','regular')"))
        .await
        .unwrap();
    let parent_conversation_id = db
        .query_one(sql(
            "SELECT COALESCE(MAX(id), 0) + 1 AS next_id FROM conversation",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i32>("", "next_id")
        .unwrap();
    db.execute(sql(format!(
        "INSERT INTO conversation (\
           id,folder_id,agent_type,status,kind,message_count,title_locked,\
           auto_title_finalized,parent_id,created_at,updated_at\
         ) VALUES (\
           {parent_conversation_id},1,'codex','completed','regular',0,0,0,NULL,\
           '2026-08-04T00:00:00Z','2026-08-04T00:00:00Z'\
         )"
    )))
    .await
    .unwrap();
    db.execute(sql(format!(
        "INSERT INTO delegation_workflows (\
           workflow_id,parent_conversation_id,workflow_kind,schema_version,\
           active_manifest_revision,graph_revision,workflow_state,capability_version,\
           publication_token,structural_revision,design_fingerprint,plan_fingerprint,\
           completion_protocol_version,completion_protocol_mode,created_at,updated_at\
         ) VALUES (\
           '{workflow_id}',{parent_conversation_id},'brainstorm_to_delivery',1,1,1,\
           'approved','workflow_manifest_v1','publication-{workflow_id}',1,\
           'design-v1','plan-v1',{completion_protocol_version},\
           '{completion_protocol_mode}','2026-08-04T00:00:00Z',\
           '2026-08-04T00:00:00Z'\
         )"
    )))
    .await
    .unwrap();
}

async fn set_legacy_source(
    db: &DatabaseConnection,
    workflow_id: &str,
    legacy_source_workflow_id: &str,
) -> Result<(), DbErr> {
    db.execute(sql(format!(
        "UPDATE delegation_workflows \
         SET legacy_source_workflow_id = '{legacy_source_workflow_id}' \
         WHERE workflow_id = '{workflow_id}'"
    )))
    .await?;
    Ok(())
}

async fn legacy_source(db: &DatabaseConnection, workflow_id: &str) -> Option<String> {
    db.query_one(sql(format!(
        "SELECT legacy_source_workflow_id FROM delegation_workflows \
         WHERE workflow_id = '{workflow_id}'"
    )))
    .await
    .unwrap()
    .unwrap()
    .try_get("", "legacy_source_workflow_id")
    .unwrap()
}

async fn insert_tool_intent(
    db: &DatabaseConnection,
    intent_id: &str,
    task_id: &str,
    child_tool_call_id: &str,
    accepted_ordinal: i64,
    request_digest: &str,
) -> Result<(), DbErr> {
    db.execute(sql(format!(
        "INSERT INTO delegation_completion_tool_intents (\
           intent_id,task_id,child_tool_call_id,accepted_ordinal,outcome,summary,\
           report_hint,request_digest,created_at\
         ) VALUES (\
           '{intent_id}','{task_id}','{child_tool_call_id}',{accepted_ordinal},\
           'done',NULL,NULL,'{request_digest}','2026-08-04T00:00:00Z'\
         )"
    )))
    .await?;
    Ok(())
}

async fn seed_open_child_question(
    db: &DatabaseConnection,
    request_id: &str,
    task_id: &str,
    parent_conversation_id: i32,
    child_conversation_id: i32,
    child_tool_call_id: &str,
    message: &str,
) {
    db.execute(sql("INSERT OR IGNORE INTO folder \
         (id,name,path,last_opened_at,created_at,updated_at,is_open,sort_order,color,kind) \
         VALUES (1,'repo','C:/completion-protocol-fixture','2026-08-04','2026-08-04',\
                 '2026-08-04',1,1,'inherit','regular')"))
        .await
        .unwrap();
    db.execute(sql(format!(
        "INSERT INTO conversation (\
           id,folder_id,agent_type,status,kind,message_count,title_locked,\
           auto_title_finalized,parent_id,created_at,updated_at\
         ) VALUES (\
           {parent_conversation_id},1,'codex','completed','regular',0,0,0,NULL,\
           '2026-08-04T00:00:00Z','2026-08-04T00:00:00Z'\
         ), (\
           {child_conversation_id},1,'codex','in_progress','delegate',0,0,0,\
           {parent_conversation_id},'2026-08-04T00:01:00Z','2026-08-04T00:01:00Z'\
         )"
    )))
    .await
    .unwrap();
    db.execute(sql(format!(
        "INSERT INTO delegation_attention_requests (\
           request_id,task_id,parent_conversation_id,child_conversation_id,\
           child_tool_call_id,status,message,reply,resolution_code,created_at,resolved_at\
         ) VALUES (\
           '{request_id}','{task_id}',{parent_conversation_id},{child_conversation_id},\
           '{child_tool_call_id}','open','{message}',NULL,NULL,\
           '2026-08-04T00:02:03.456Z',NULL\
         )"
    )))
    .await
    .unwrap();
}

async fn seed_resolved_child_question(
    db: &DatabaseConnection,
    request_id: &str,
    task_id: &str,
    parent_conversation_id: i32,
    child_conversation_id: i32,
    child_tool_call_id: &str,
) {
    db.execute(sql(format!(
        "INSERT INTO delegation_attention_requests (\
           request_id,task_id,parent_conversation_id,child_conversation_id,\
           child_tool_call_id,status,message,reply,resolution_code,created_at,resolved_at\
         ) VALUES (\
           '{request_id}','{task_id}',{parent_conversation_id},{child_conversation_id},\
           '{child_tool_call_id}','resolved','Historical question','Historical reply',\
           'parent_reply','2026-08-04T00:03:04.567Z','2026-08-04T00:04:05.678Z'\
         )"
    )))
    .await
    .unwrap();
}

async fn child_question_projection(
    db: &DatabaseConnection,
    request_id: &str,
) -> ChildQuestionProjection {
    let row = db
        .query_one(sql(format!(
            "SELECT request_id,task_id,parent_conversation_id,child_conversation_id,\
                    child_tool_call_id,status,message,reply,resolution_code,created_at,resolved_at \
             FROM delegation_attention_requests WHERE request_id = '{request_id}'"
        )))
        .await
        .unwrap()
        .unwrap();
    ChildQuestionProjection {
        request_id: row.try_get("", "request_id").unwrap(),
        task_id: row.try_get("", "task_id").unwrap(),
        parent_conversation_id: row.try_get("", "parent_conversation_id").unwrap(),
        child_conversation_id: row.try_get("", "child_conversation_id").unwrap(),
        child_tool_call_id: row.try_get("", "child_tool_call_id").unwrap(),
        status: row.try_get("", "status").unwrap(),
        message: row.try_get("", "message").unwrap(),
        reply: row.try_get("", "reply").unwrap(),
        resolution_code: row.try_get("", "resolution_code").unwrap(),
        created_at: row.try_get("", "created_at").unwrap(),
        resolved_at: row.try_get("", "resolved_at").unwrap(),
    }
}

async fn attention_kind(db: &DatabaseConnection, request_id: &str) -> String {
    db.query_one(sql(format!(
        "SELECT kind FROM delegation_attention_requests WHERE request_id = '{request_id}'"
    )))
    .await
    .unwrap()
    .unwrap()
    .try_get("", "kind")
    .unwrap()
}

async fn insert_completion_attention(
    db: &DatabaseConnection,
    request_id: &str,
    task_id: &str,
    kind: &str,
    child_conversation_id: Option<i32>,
    child_tool_call_id: Option<&str>,
) -> Result<(), DbErr> {
    let child_conversation_id = child_conversation_id
        .map(|value| value.to_string())
        .unwrap_or_else(|| "NULL".to_owned());
    let child_tool_call_id = child_tool_call_id
        .map(|value| format!("'{value}'"))
        .unwrap_or_else(|| "NULL".to_owned());
    db.execute(sql(format!(
        "INSERT INTO delegation_attention_requests (\
           request_id,task_id,parent_conversation_id,child_conversation_id,\
           child_tool_call_id,status,message,kind,created_at\
         ) VALUES (\
           '{request_id}','{task_id}',10,{child_conversation_id},{child_tool_call_id},\
           'open','typed attention','{kind}','2026-08-04T01:00:00Z'\
         )"
    )))
    .await?;
    Ok(())
}

async fn insert_outbox_event(
    db: &DatabaseConnection,
    event_id: &str,
    workflow_id: &str,
    graph_revision: i64,
    event_kind: &str,
    subject_key: &str,
) -> Result<(), DbErr> {
    db.execute(sql(format!(
        "INSERT INTO delegation_workflow_outbox_events (\
           event_id,workflow_id,graph_revision,event_kind,subject_key,payload_json,created_at\
         ) VALUES (\
           '{event_id}','{workflow_id}',{graph_revision},'{event_kind}','{subject_key}',\
           '{{\"version\":1}}','2026-08-04T01:00:00Z'\
         )"
    )))
    .await?;
    Ok(())
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

#[tokio::test]
async fn migration_3_enforces_tool_redelivery_and_one_restart_successor() {
    let db = open_through(MIGRATION_2).await;
    seed_workflow(&db, "legacy", 1, "v1").await;
    seed_workflow(&db, "successor-a", 2, "v2_enforce").await;
    seed_workflow(&db, "successor-b", 2, "v2_enforce").await;

    apply_named(&db, MIGRATION_3).await.unwrap();

    let intent_columns = table_columns(&db, "delegation_completion_tool_intents").await;
    assert_eq!(
        intent_columns
            .iter()
            .map(|column| (column.name.as_str(), column.not_null))
            .collect::<Vec<_>>(),
        vec![
            ("intent_id", true),
            ("task_id", true),
            ("child_tool_call_id", true),
            ("accepted_ordinal", true),
            ("outcome", true),
            ("summary", false),
            ("report_hint", false),
            ("request_digest", true),
            ("created_at", true),
        ]
    );
    let intent_indexes = table_index_sql(&db, "delegation_completion_tool_intents").await;
    assert!(intent_indexes
        .get("idx_dcti_task_latest")
        .and_then(Option::as_deref)
        .is_some_and(|definition| definition.contains("accepted_ordinal DESC")));

    set_legacy_source(&db, "successor-a", "legacy")
        .await
        .unwrap();
    assert!(set_legacy_source(&db, "successor-b", "legacy")
        .await
        .is_err());

    insert_tool_intent(&db, "intent-1", "task-1", "rpc:inc:7", 1, "sha256:a")
        .await
        .unwrap();
    assert!(
        insert_tool_intent(&db, "intent-2", "task-1", "rpc:inc:7", 2, "sha256:a")
            .await
            .is_err()
    );
    assert!(
        insert_tool_intent(&db, "intent-3", "task-1", "rpc:inc:8", 1, "sha256:b")
            .await
            .is_err()
    );
    insert_tool_intent(&db, "intent-4", "task-1", "rpc:inc:8", 2, "sha256:b")
        .await
        .unwrap();
}

#[tokio::test]
async fn migration_3_leaves_existing_workflows_without_restart_links() {
    let db = open_through(MIGRATION_2).await;
    seed_workflow(&db, "existing", 1, "v1").await;

    apply_named(&db, MIGRATION_3).await.unwrap();

    assert_eq!(legacy_source(&db, "existing").await, None);
}

#[test]
fn migration_3_is_registered_immediately_after_migration_2() {
    let names: Vec<String> = Migrator::migrations()
        .into_iter()
        .map(|migration| migration.name().to_owned())
        .collect();
    let second = names.iter().position(|name| name == MIGRATION_2).unwrap();
    let third = names.iter().position(|name| name == MIGRATION_3).unwrap();
    assert_eq!(third, second + 1);
}

#[tokio::test]
async fn migration_4_preserves_child_questions_and_permits_typed_terminal_rows() {
    assert!(
        Migrator::migrations()
            .iter()
            .any(|migration| migration.name() == MIGRATION_4),
        "missing migration {MIGRATION_4}"
    );

    let db = open_through(MIGRATION_3).await;
    seed_open_child_question(&db, "request-1", "task-1", 10, 11, "tool-1", "Choose A").await;
    seed_resolved_child_question(&db, "request-history", "task-history", 10, 11, "tool-2").await;
    seed_workflow(&db, "wf-outbox", 2, "v2_enforce").await;
    let before_open = child_question_projection(&db, "request-1").await;
    let before_resolved = child_question_projection(&db, "request-history").await;

    apply_named(&db, MIGRATION_4).await.unwrap();

    assert_eq!(
        child_question_projection(&db, "request-1").await,
        before_open
    );
    assert_eq!(
        child_question_projection(&db, "request-history").await,
        before_resolved
    );
    assert_eq!(attention_kind(&db, "request-1").await, "child_question");
    assert_eq!(
        attention_kind(&db, "request-history").await,
        "child_question"
    );

    insert_completion_attention(
        &db,
        "request-2",
        "task-1",
        "completion_decision",
        None,
        None,
    )
    .await
    .unwrap();
    assert!(insert_completion_attention(
        &db,
        "request-3",
        "task-1",
        "completion_decision",
        None,
        None,
    )
    .await
    .is_err());
    insert_completion_attention(
        &db,
        "request-4",
        "task-1",
        "completion_artifact_recovery",
        None,
        None,
    )
    .await
    .unwrap();
    assert!(insert_completion_attention(
        &db,
        "request-invalid-design",
        "task-design-invalid",
        "design_self_review_decision",
        Some(11),
        None,
    )
    .await
    .is_err());
    insert_completion_attention(
        &db,
        "request-design",
        "task-1",
        "design_self_review_decision",
        None,
        None,
    )
    .await
    .unwrap();
    assert!(insert_completion_attention(
        &db,
        "request-invalid-child",
        "task-child-invalid",
        "child_question",
        None,
        None,
    )
    .await
    .is_err());
    assert!(insert_completion_attention(
        &db,
        "request-invalid-kind",
        "task-kind-invalid",
        "free_form",
        None,
        None,
    )
    .await
    .is_err());
    insert_completion_attention(
        &db,
        "request-tool-a",
        "task-tool",
        "completion_decision",
        Some(11),
        Some("typed-tool"),
    )
    .await
    .unwrap();
    assert!(insert_completion_attention(
        &db,
        "request-tool-b",
        "task-tool",
        "completion_artifact_recovery",
        Some(11),
        Some("typed-tool"),
    )
    .await
    .is_err());

    let attention_table_sql = table_sql(&db, "delegation_attention_requests").await;
    for kind in [
        "'child_question'",
        "'completion_decision'",
        "'completion_artifact_recovery'",
        "'design_self_review_decision'",
    ] {
        assert!(
            attention_table_sql.contains(kind),
            "missing attention kind {kind}"
        );
    }
    let attention_indexes = table_index_sql(&db, "delegation_attention_requests").await;
    assert!(!attention_indexes.contains_key("idx_attention_one_open_per_task"));
    assert!(attention_indexes.values().any(|definition| {
        definition.as_deref().is_some_and(|sql| {
            sql.contains("task_id, kind") && sql.contains("WHERE status = 'open'")
        })
    }));
    assert!(attention_indexes.values().any(|definition| {
        definition.as_deref().is_some_and(|sql| {
            sql.contains("task_id, child_tool_call_id")
                && sql.contains("WHERE child_tool_call_id IS NOT NULL")
        })
    }));

    assert!(table_exists(&db, "delegation_workflow_outbox_events").await);
    let outbox_columns = table_columns(&db, "delegation_workflow_outbox_events").await;
    assert_eq!(
        outbox_columns
            .iter()
            .map(|column| (column.name.as_str(), column.not_null))
            .collect::<Vec<_>>(),
        vec![
            ("event_id", true),
            ("workflow_id", true),
            ("graph_revision", true),
            ("event_kind", true),
            ("subject_key", true),
            ("payload_json", true),
            ("dispatch_attempts", true),
            ("created_at", true),
            ("delivered_at", false),
        ]
    );
    insert_outbox_event(&db, "event-1", "wf-outbox", 7, "attention_opened", "task-1")
        .await
        .unwrap();
    assert!(
        insert_outbox_event(&db, "event-2", "wf-outbox", 7, "attention_opened", "task-1",)
            .await
            .is_err()
    );
    insert_outbox_event(&db, "event-3", "wf-outbox", 8, "attention_opened", "task-1")
        .await
        .unwrap();
    let attempts = db
        .query_one(sql(
            "SELECT dispatch_attempts FROM delegation_workflow_outbox_events \
             WHERE event_id = 'event-1'",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "dispatch_attempts")
        .unwrap();
    assert_eq!(attempts, 0);
    assert_foreign_key_check_clean(&db).await;
}

#[tokio::test]
async fn migration_4_rolls_back_attention_rebuild_failures() {
    assert!(
        Migrator::migrations()
            .iter()
            .any(|migration| migration.name() == MIGRATION_4),
        "missing migration {MIGRATION_4}"
    );

    for failpoint in ["copy", "schema", "index", "foreign_key_check"] {
        let db = open_through(MIGRATION_3).await;
        seed_open_child_question(&db, "request-1", "task-1", 10, 11, "tool-1", "Choose A").await;
        let before_row = child_question_projection(&db, "request-1").await;
        let before_table_sql = table_sql(&db, "delegation_attention_requests").await;
        let before_columns = table_columns(&db, "delegation_attention_requests").await;
        let before_foreign_keys = table_foreign_keys(&db, "delegation_attention_requests").await;
        let before_indexes = table_indexes(&db, "delegation_attention_requests").await;
        let before_index_sql = table_index_sql(&db, "delegation_attention_requests").await;

        assert!(
            run_attention_rebuild_with_failpoint(&db, failpoint)
                .await
                .is_err(),
            "{failpoint} did not fail"
        );
        assert_eq!(
            child_question_projection(&db, "request-1").await,
            before_row,
            "{failpoint} changed row bytes"
        );
        assert_eq!(
            table_sql(&db, "delegation_attention_requests").await,
            before_table_sql,
            "{failpoint} changed table SQL"
        );
        assert_eq!(
            table_columns(&db, "delegation_attention_requests").await,
            before_columns,
            "{failpoint} changed table_info"
        );
        assert_eq!(
            table_foreign_keys(&db, "delegation_attention_requests").await,
            before_foreign_keys,
            "{failpoint} changed foreign keys"
        );
        assert_eq!(
            table_indexes(&db, "delegation_attention_requests").await,
            before_indexes,
            "{failpoint} changed indexes"
        );
        assert_eq!(
            table_index_sql(&db, "delegation_attention_requests").await,
            before_index_sql,
            "{failpoint} changed index SQL"
        );
        assert!(!table_exists(&db, "delegation_attention_requests_v2").await);
        assert!(!table_exists(&db, "delegation_workflow_outbox_events").await);
        assert_foreign_key_check_clean(&db).await;
    }
}

#[test]
fn migration_4_is_registered_immediately_after_migration_3() {
    let names: Vec<String> = Migrator::migrations()
        .into_iter()
        .map(|migration| migration.name().to_owned())
        .collect();
    let third = names.iter().position(|name| name == MIGRATION_3).unwrap();
    let fourth = names.iter().position(|name| name == MIGRATION_4).unwrap();
    assert_eq!(fourth, third + 1);
}

async fn open_before_completion_protocol_v2_only() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    db.execute_unprepared("PRAGMA foreign_keys = ON")
        .await
        .unwrap();
    BeforeCompletionProtocolV2Only::up(&db, None).await.unwrap();
    db
}

async fn seed_trigger_folder(db: &DatabaseConnection) {
    db.execute(sql("INSERT INTO folder \
         (id,name,path,last_opened_at,created_at,updated_at,is_open,sort_order,color,kind) \
         VALUES (1,'repo','C:/completion-protocol-v2-only','2026-08-09','2026-08-09',\
                 '2026-08-09',1,1,'inherit','regular')"))
        .await
        .unwrap();
}

async fn seed_trigger_conversation(db: &DatabaseConnection, conversation_id: i32) {
    db.execute(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "INSERT INTO conversation (\
           id,folder_id,agent_type,status,kind,message_count,title_locked,\
           auto_title_finalized,parent_id,created_at,updated_at\
         ) VALUES (?,1,'codex','completed','regular',0,0,0,NULL,\
                   '2026-08-09T00:00:00Z','2026-08-09T00:00:00Z')",
        vec![conversation_id.into()],
    ))
    .await
    .unwrap();
}

async fn insert_trigger_workflow(
    db: &DatabaseConnection,
    workflow_id: &str,
    parent_conversation_id: i32,
    version: i64,
    mode: &str,
    legacy_source_workflow_id: Option<&str>,
) -> Result<(), DbErr> {
    db.execute(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "INSERT INTO delegation_workflows (\
           workflow_id,parent_conversation_id,workflow_kind,schema_version,\
           active_manifest_revision,graph_revision,workflow_state,capability_version,\
           publication_token,structural_revision,design_fingerprint,plan_fingerprint,\
           completion_protocol_version,completion_protocol_mode,\
           legacy_source_workflow_id,created_at,updated_at\
         ) VALUES (?,?, 'brainstorm_to_delivery',1,1,1,'approved',\
                   'workflow_manifest_v1',?,1,'design-v1','plan-v1',?,?,?,\
                   '2026-08-09T00:00:00Z','2026-08-09T00:00:00Z')",
        vec![
            workflow_id.into(),
            parent_conversation_id.into(),
            format!("publication-{workflow_id}").into(),
            version.into(),
            mode.into(),
            legacy_source_workflow_id.into(),
        ],
    ))
    .await?;
    Ok(())
}

async fn insert_trigger_workflow_without_protocol(
    db: &DatabaseConnection,
    workflow_id: &str,
    parent_conversation_id: i32,
) -> Result<(), DbErr> {
    db.execute(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "INSERT INTO delegation_workflows (\
           workflow_id,parent_conversation_id,workflow_kind,schema_version,\
           active_manifest_revision,graph_revision,workflow_state,capability_version,\
           publication_token,structural_revision,design_fingerprint,plan_fingerprint,\
           created_at,updated_at\
         ) VALUES (?,?, 'brainstorm_to_delivery',1,1,1,'approved',\
                   'workflow_manifest_v1',?,1,'design-v1','plan-v1',\
                   '2026-08-09T00:00:00Z','2026-08-09T00:00:00Z')",
        vec![
            workflow_id.into(),
            parent_conversation_id.into(),
            format!("publication-{workflow_id}").into(),
        ],
    ))
    .await?;
    Ok(())
}

async fn seed_trigger_cascade_dependents(
    db: &DatabaseConnection,
    workflow_id: &str,
    parent_conversation_id: i32,
    child_conversation_id: i32,
) {
    db.execute(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "INSERT INTO delegation_workflow_manifest_revisions (\
           workflow_id,manifest_revision,manifest_state,document_json,document_digest,created_at\
         ) VALUES (?,1,'approved','{\"schema_version\":1}','sha256:manifest',\
                   '2026-08-09T00:00:00Z')",
        vec![workflow_id.into()],
    ))
    .await
    .unwrap();
    db.execute(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "INSERT INTO delegation_task_runs (\
           task_id,root_task_id,generation,parent_conversation_id,child_conversation_id,\
           agent_type,admission_class,lineage_root_task_id,history_only,status,\
           completion_state,completion_outcome,completion_evidence_json,created_at,updated_at\
         ) VALUES (?,?,1,?,?,'codex','normal_revision',?,0,'completed',\
                   'resolved','done','{\"evidence\":[\"historical\"]}',\
                   '2026-08-09T00:00:00Z','2026-08-09T00:00:00Z')",
        vec![
            format!("task-{workflow_id}").into(),
            format!("task-{workflow_id}").into(),
            parent_conversation_id.into(),
            child_conversation_id.into(),
            format!("task-{workflow_id}").into(),
        ],
    ))
    .await
    .unwrap();
}

async fn workflow_rows_snapshot(db: &DatabaseConnection) -> Vec<String> {
    let columns = table_columns(db, "delegation_workflows").await;
    let projection = columns
        .iter()
        .map(|column| format!("quote({})", column.name))
        .collect::<Vec<_>>()
        .join(" || char(31) || ");
    db.query_all(sql(format!(
        "SELECT {projection} AS snapshot FROM delegation_workflows ORDER BY workflow_id"
    )))
    .await
    .unwrap()
    .into_iter()
    .map(|row| row.try_get("", "snapshot").unwrap())
    .collect()
}

async fn trigger_names(db: &DatabaseConnection) -> Vec<String> {
    db.query_all(sql(
        "SELECT name FROM sqlite_master WHERE type = 'trigger' ORDER BY name",
    ))
    .await
    .unwrap()
    .into_iter()
    .map(|row| row.try_get("", "name").unwrap())
    .collect()
}

async fn assert_statement_aborts_with(
    db: &DatabaseConnection,
    statement: Statement,
    expected_marker: &str,
) {
    let error = db
        .execute(statement)
        .await
        .expect_err("statement unexpectedly succeeded");
    assert!(
        error.to_string().contains(expected_marker),
        "expected {expected_marker}, got {error}"
    );
}

async fn count_rows_by_id(db: &DatabaseConnection, table: &str, column: &str, value: &str) -> i64 {
    db.query_one(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        format!("SELECT COUNT(*) AS count FROM {table} WHERE {column} = ?"),
        vec![value.into()],
    ))
    .await
    .unwrap()
    .unwrap()
    .try_get("", "count")
    .unwrap()
}

#[test]
fn v2_only_trigger_migration_is_registered_after_legacy_restart_context() {
    let names: Vec<String> = Migrator::migrations()
        .into_iter()
        .map(|migration| migration.name().to_owned())
        .collect();
    let predecessor = names
        .iter()
        .position(|name| name == PRE_V2_ONLY_MIGRATION)
        .unwrap();
    let v2_only = names
        .iter()
        .position(|name| name == MIGRATION_V2_ONLY)
        .expect("missing completion-protocol-v2-only migration");
    let between: Vec<&str> = names[predecessor + 1..v2_only]
        .iter()
        .map(String::as_str)
        .collect();
    assert_eq!(between.as_slice(), FORK_MIGRATIONS_BEFORE_V2_ONLY);
}

#[tokio::test]
async fn v2_only_trigger_matrix_preserves_history_and_freezes_writes() {
    let db = open_before_completion_protocol_v2_only().await;
    seed_trigger_folder(&db).await;
    for conversation_id in [101, 102, 103, 104] {
        seed_trigger_conversation(&db, conversation_id).await;
    }

    insert_trigger_workflow(&db, "wf-historical", 101, 1, "v1", None)
        .await
        .unwrap();
    insert_trigger_workflow(
        &db,
        "wf-linked-successor",
        102,
        2,
        "v2_enforce",
        Some("wf-historical"),
    )
    .await
    .unwrap();
    insert_trigger_workflow(&db, "wf-delete-cascade", 103, 1, "v1", None)
        .await
        .unwrap();
    seed_trigger_cascade_dependents(&db, "wf-delete-cascade", 103, 104).await;

    let historical_before_up = workflow_rows_snapshot(&db).await;
    Migrator::up(&db, None).await.unwrap();
    assert_eq!(
        workflow_rows_snapshot(&db).await,
        historical_before_up,
        "migration up must not rewrite historical headers or links"
    );

    for conversation_id in 110..=119 {
        seed_trigger_conversation(&db, conversation_id).await;
    }

    let omitted = insert_trigger_workflow_without_protocol(&db, "wf-omitted", 110)
        .await
        .expect_err("insert with omitted protocol columns unexpectedly succeeded");
    assert!(
        omitted.to_string().contains("completion_protocol_v2_only"),
        "unexpected omitted-protocol insert error: {omitted}"
    );

    for (index, version, mode) in [
        (111, 1, "v1"),
        (112, 1, "v2_shadow"),
        (113, 1, "v2_enforce"),
        (114, 2, "v1"),
        (115, 2, "v2_shadow"),
    ] {
        let error = insert_trigger_workflow(
            &db,
            &format!("wf-rejected-{index}"),
            index,
            version,
            mode,
            None,
        )
        .await
        .expect_err("non-v2-only insert unexpectedly succeeded");
        assert!(
            error.to_string().contains("completion_protocol_v2_only"),
            "unexpected insert error: {error}"
        );
    }

    insert_trigger_workflow(&db, "wf-current", 117, 2, "v2_enforce", None)
        .await
        .unwrap();
    let legacy_insert = insert_trigger_workflow(
        &db,
        "wf-v2-with-legacy-source",
        118,
        2,
        "v2_enforce",
        Some("wf-historical"),
    )
    .await
    .expect_err("new v2 workflow accepted a legacy source");
    assert!(
        legacy_insert
            .to_string()
            .contains("completion_protocol_v2_only"),
        "unexpected legacy-source insert error: {legacy_insert}"
    );

    for (workflow_id, version, mode) in [
        ("wf-historical", 2, "v2_shadow"),
        ("wf-current", 1, "v2_shadow"),
    ] {
        assert_statement_aborts_with(
            &db,
            Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "UPDATE delegation_workflows SET completion_protocol_version = ? \
                 WHERE workflow_id = ?",
                vec![version.into(), workflow_id.into()],
            ),
            "completion_protocol_frozen",
        )
        .await;
        assert_statement_aborts_with(
            &db,
            Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "UPDATE delegation_workflows SET completion_protocol_mode = ? \
                 WHERE workflow_id = ?",
                vec![mode.into(), workflow_id.into()],
            ),
            "completion_protocol_frozen",
        )
        .await;
    }

    for workflow_id in ["wf-historical", "wf-current"] {
        db.execute(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "UPDATE delegation_workflows \
             SET graph_revision = graph_revision + 1,\
                 updated_at = '2026-08-09T01:00:00Z',\
                 completion_protocol_version = completion_protocol_version,\
                 completion_protocol_mode = completion_protocol_mode \
             WHERE workflow_id = ?",
            vec![workflow_id.into()],
        ))
        .await
        .unwrap();
        db.execute(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "UPDATE delegation_workflows \
             SET graph_revision = graph_revision + 1,\
                 updated_at = '2026-08-09T02:00:00Z' \
             WHERE workflow_id = ?",
            vec![workflow_id.into()],
        ))
        .await
        .unwrap();
    }

    assert_statement_aborts_with(
        &db,
        Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "UPDATE delegation_workflows SET legacy_source_workflow_id = ? \
             WHERE workflow_id = 'wf-historical'",
            vec!["wf-linked-successor".into()],
        ),
        "legacy_source_workflow_frozen",
    )
    .await;
    assert_statement_aborts_with(
        &db,
        sql(
            "UPDATE delegation_workflows SET legacy_source_workflow_id = NULL \
             WHERE workflow_id = 'wf-linked-successor'",
        ),
        "legacy_source_workflow_frozen",
    )
    .await;
    assert_statement_aborts_with(
        &db,
        Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "UPDATE delegation_workflows SET legacy_source_workflow_id = ? \
             WHERE workflow_id = 'wf-linked-successor'",
            vec!["wf-other-source".into()],
        ),
        "legacy_source_workflow_frozen",
    )
    .await;
    db.execute(sql("UPDATE delegation_workflows \
         SET legacy_source_workflow_id = legacy_source_workflow_id \
         WHERE workflow_id = 'wf-linked-successor'"))
        .await
        .unwrap();
    assert_eq!(
        legacy_source(&db, "wf-linked-successor").await.as_deref(),
        Some("wf-historical")
    );

    db.execute(sql("DELETE FROM conversation WHERE id = 103"))
        .await
        .unwrap();
    assert_eq!(
        count_rows_by_id(
            &db,
            "delegation_workflows",
            "workflow_id",
            "wf-delete-cascade"
        )
        .await,
        0
    );
    assert_eq!(
        count_rows_by_id(
            &db,
            "delegation_workflow_manifest_revisions",
            "workflow_id",
            "wf-delete-cascade"
        )
        .await,
        0
    );
    assert_eq!(
        count_rows_by_id(
            &db,
            "delegation_task_runs",
            "task_id",
            "task-wf-delete-cascade"
        )
        .await,
        0
    );

    db.execute_unprepared(
        "CREATE TRIGGER trg_task7_sentinel \
         BEFORE UPDATE OF workflow_state ON delegation_workflows \
         WHEN 0 BEGIN SELECT 1; END",
    )
    .await
    .unwrap();
    let triggers_before_down = trigger_names(&db).await;
    for trigger in V2_ONLY_TRIGGERS {
        assert!(
            triggers_before_down.iter().any(|name| name == trigger),
            "missing trigger {trigger}"
        );
    }
    let rows_before_down = workflow_rows_snapshot(&db).await;

    let migration = Migrator::migrations()
        .into_iter()
        .find(|migration| migration.name() == MIGRATION_V2_ONLY)
        .expect("missing completion-protocol-v2-only migration");
    migration.down(&SchemaManager::new(&db)).await.unwrap();

    let expected_triggers_after_down = triggers_before_down
        .into_iter()
        .filter(|name| !V2_ONLY_TRIGGERS.contains(&name.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(trigger_names(&db).await, expected_triggers_after_down);
    assert_eq!(workflow_rows_snapshot(&db).await, rows_before_down);
    assert_eq!(
        legacy_source(&db, "wf-linked-successor").await.as_deref(),
        Some("wf-historical")
    );

    insert_trigger_workflow(&db, "wf-v1-after-down", 119, 1, "v1", None)
        .await
        .expect("down must remove the v2-only insert trigger");
    assert_foreign_key_check_clean(&db).await;
}
