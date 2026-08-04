use std::collections::HashSet;

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{DbBackend, Statement};

pub(super) const V1_SETTLEMENT_COLUMNS: &[&str] = &[
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

const SOURCE_TABLE: &str = "delegation_workflow_gate_settlements";
const REPLACEMENT_TABLE: &str = "delegation_workflow_gate_settlements_v2";

const V1_ATTENTION_COLUMNS: &[&str] = &[
    "request_id",
    "task_id",
    "parent_conversation_id",
    "child_conversation_id",
    "child_tool_call_id",
    "status",
    "message",
    "reply",
    "resolution_code",
    "created_at",
    "resolved_at",
];

const ATTENTION_SOURCE_TABLE: &str = "delegation_attention_requests";
const ATTENTION_REPLACEMENT_TABLE: &str = "delegation_attention_requests_v2";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RebuildFailpoint {
    None,
    Copy,
    Schema,
    Index,
    ForeignKeyCheck,
}

#[cfg(any(test, feature = "test-utils"))]
pub(super) async fn configured_failpoint<C: ConnectionTrait>(
    conn: &C,
) -> Result<RebuildFailpoint, DbErr> {
    const FAILPOINT_COPY: i64 = 0x434f_5001;
    const FAILPOINT_SCHEMA: i64 = 0x5343_4801;
    const FAILPOINT_INDEX: i64 = 0x494e_4401;
    const FAILPOINT_FOREIGN_KEY_CHECK: i64 = 0x464b_4301;

    let value = conn
        .query_one(Statement::from_string(
            DbBackend::Sqlite,
            "PRAGMA application_id".to_owned(),
        ))
        .await?
        .ok_or_else(|| DbErr::Custom("PRAGMA application_id returned no row".to_owned()))?
        .try_get::<i64>("", "application_id")?;

    Ok(match value {
        FAILPOINT_COPY => RebuildFailpoint::Copy,
        FAILPOINT_SCHEMA => RebuildFailpoint::Schema,
        FAILPOINT_INDEX => RebuildFailpoint::Index,
        FAILPOINT_FOREIGN_KEY_CHECK => RebuildFailpoint::ForeignKeyCheck,
        _ => RebuildFailpoint::None,
    })
}

pub(super) async fn rebuild_gate_settlements<C: ConnectionTrait>(
    conn: &C,
    failpoint: RebuildFailpoint,
) -> Result<(), DbErr> {
    let source_table_sql = load_table_sql(conn, SOURCE_TABLE).await?;
    let source_indexes = load_index_sql(conn, SOURCE_TABLE).await?;
    let source_columns = load_column_names(conn, SOURCE_TABLE).await?;

    conn.execute_unprepared(
        r#"CREATE TABLE delegation_workflow_gate_settlements_v2 (
           workflow_id TEXT NOT NULL,
           gate_id TEXT NOT NULL,
           gate_cycle INTEGER NOT NULL,
           manifest_revision INTEGER NOT NULL,
           structural_revision INTEGER NOT NULL DEFAULT 1,
           content_fingerprint TEXT NOT NULL DEFAULT '',
           outcome TEXT NOT NULL CHECK (outcome IN (
             'approved','changes_requested','blocked'
           )),
           critical_count INTEGER NULL,
           important_count INTEGER NULL,
           minor_count INTEGER NULL,
           summary TEXT NOT NULL CHECK (length(summary) <= 4096),
           graph_revision_at_settle INTEGER NOT NULL,
           review_scope TEXT NULL CHECK (
             review_scope IS NULL OR review_scope IN ('full','scoped')
           ),
           revision_kind TEXT NULL CHECK (
             revision_kind IS NULL OR revision_kind IN (
               'initial','localized','material','holistic_rewrite'
             )
           ),
           scope_reason TEXT NULL,
           required_reviewer_node_ids_json TEXT NULL,
           covered_author_task_id TEXT NULL,
           covered_plan_digest TEXT NULL,
           finding_ledger_json TEXT NULL,
           net_improvement INTEGER NULL CHECK (
             net_improvement IS NULL OR net_improvement IN (0, 1)
           ),
           stagnation_count INTEGER NOT NULL DEFAULT 0 CHECK (
             stagnation_count >= 0
           ),
           rewrite_used INTEGER NOT NULL DEFAULT 0 CHECK (
             rewrite_used IN (0, 1)
           ),
           next_action TEXT NULL CHECK (
             next_action IS NULL OR next_action IN (
               'continue_review','holistic_rewrite_required',
               'user_decision_required','approved'
             )
           ),
           report_files_json TEXT NULL,
           lineage_reset_authorization_id TEXT NULL,
           created_at TEXT NOT NULL,
           evidence_scope_digest TEXT NULL,
           gate_lineage TEXT NULL,
           review_round INTEGER NULL,
           required_node_set_json TEXT NULL,
           required_evidence_task_ids_json TEXT NULL,
           evidence_scope_digests_json TEXT NULL,
           localized_change_digest TEXT NULL,
           plan_round_state_v2_json TEXT NULL,
           FOREIGN KEY(workflow_id)
             REFERENCES delegation_workflows(workflow_id) ON DELETE CASCADE
         )"#,
    )
    .await?;

    let destination_columns = V1_SETTLEMENT_COLUMNS
        .iter()
        .chain(V2_SETTLEMENT_COLUMNS)
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    let source_values = V1_SETTLEMENT_COLUMNS
        .iter()
        .map(|column| source_value_expression(column, &source_columns))
        .chain(V2_SETTLEMENT_COLUMNS.iter().map(|_| "NULL".to_owned()))
        .collect::<Vec<_>>()
        .join(", ");
    conn.execute_unprepared(&format!(
        "INSERT INTO {REPLACEMENT_TABLE} ({destination_columns}) \
         SELECT {source_values} FROM {SOURCE_TABLE}"
    ))
    .await?;
    fail_at(
        failpoint,
        RebuildFailpoint::Copy,
        "copy",
        &source_table_sql,
        &source_indexes,
    )?;

    verify_projection(conn, &source_columns, &source_table_sql, &source_indexes).await?;

    conn.execute_unprepared(&format!("DROP TABLE {SOURCE_TABLE}"))
        .await?;
    conn.execute_unprepared(&format!(
        "ALTER TABLE {REPLACEMENT_TABLE} RENAME TO {SOURCE_TABLE}"
    ))
    .await?;
    fail_at(
        failpoint,
        RebuildFailpoint::Schema,
        "schema",
        &source_table_sql,
        &source_indexes,
    )?;

    for (_, definition) in &source_indexes {
        conn.execute_unprepared(definition).await?;
    }
    fail_at(
        failpoint,
        RebuildFailpoint::Index,
        "index",
        &source_table_sql,
        &source_indexes,
    )?;

    verify_foreign_keys(conn).await?;
    fail_at(
        failpoint,
        RebuildFailpoint::ForeignKeyCheck,
        "foreign_key_check",
        &source_table_sql,
        &source_indexes,
    )?;

    Ok(())
}

pub(super) async fn rebuild_attention_requests_and_outbox<C: ConnectionTrait>(
    conn: &C,
    failpoint: RebuildFailpoint,
) -> Result<(), DbErr> {
    let source_table_sql = load_table_sql(conn, ATTENTION_SOURCE_TABLE).await?;
    let source_indexes = load_index_sql(conn, ATTENTION_SOURCE_TABLE).await?;

    conn.execute_unprepared(
        r#"CREATE TABLE delegation_attention_requests_v2 (
           request_id TEXT PRIMARY KEY NOT NULL,
           task_id TEXT NOT NULL,
           parent_conversation_id INTEGER NOT NULL,
           child_conversation_id INTEGER NULL,
           child_tool_call_id TEXT NULL,
           status TEXT NOT NULL CHECK (status IN ('open','resolved')),
           message TEXT NOT NULL,
           reply TEXT NULL,
           resolution_code TEXT NULL,
           created_at TEXT NOT NULL,
           resolved_at TEXT NULL,
           kind TEXT NOT NULL DEFAULT 'child_question' CHECK(kind IN (
             'child_question',
             'completion_decision',
             'completion_artifact_recovery',
             'design_self_review_decision'
           )),
           latest_run_id TEXT NULL,
           node_id TEXT NULL,
           payload_json TEXT NULL,
           resolution_json TEXT NULL,
           captured_scope_digest TEXT NULL,
           CHECK(kind != 'child_question' OR
             (child_conversation_id IS NOT NULL AND child_tool_call_id IS NOT NULL)),
           CHECK(kind != 'design_self_review_decision' OR child_conversation_id IS NULL),
           FOREIGN KEY(parent_conversation_id)
             REFERENCES conversation(id) ON DELETE CASCADE,
           FOREIGN KEY(child_conversation_id)
             REFERENCES conversation(id) ON DELETE CASCADE
         )"#,
    )
    .await?;

    let destination_columns = V1_ATTENTION_COLUMNS
        .iter()
        .copied()
        .chain([
            "kind",
            "latest_run_id",
            "node_id",
            "payload_json",
            "resolution_json",
            "captured_scope_digest",
        ])
        .collect::<Vec<_>>()
        .join(", ");
    let source_values = V1_ATTENTION_COLUMNS
        .iter()
        .copied()
        .chain(["'child_question'", "NULL", "NULL", "NULL", "NULL", "NULL"])
        .collect::<Vec<_>>()
        .join(", ");
    conn.execute_unprepared(&format!(
        "INSERT INTO {ATTENTION_REPLACEMENT_TABLE} ({destination_columns}) \
         SELECT {source_values} FROM {ATTENTION_SOURCE_TABLE}"
    ))
    .await?;
    fail_attention_at(
        failpoint,
        RebuildFailpoint::Copy,
        "copy",
        &source_table_sql,
        &source_indexes,
    )?;

    verify_attention_projection(conn, &source_table_sql, &source_indexes).await?;

    conn.execute_unprepared(&format!("DROP TABLE {ATTENTION_SOURCE_TABLE}"))
        .await?;
    conn.execute_unprepared(&format!(
        "ALTER TABLE {ATTENTION_REPLACEMENT_TABLE} RENAME TO {ATTENTION_SOURCE_TABLE}"
    ))
    .await?;
    fail_attention_at(
        failpoint,
        RebuildFailpoint::Schema,
        "schema",
        &source_table_sql,
        &source_indexes,
    )?;

    for (name, definition) in &source_indexes {
        if matches!(
            name.as_str(),
            "idx_attention_task_tool_call" | "idx_attention_one_open_per_task"
        ) {
            continue;
        }
        conn.execute_unprepared(definition).await?;
    }
    for statement in [
        r#"CREATE UNIQUE INDEX idx_attention_task_tool_call
           ON delegation_attention_requests(task_id, child_tool_call_id)
           WHERE child_tool_call_id IS NOT NULL"#,
        r#"CREATE UNIQUE INDEX idx_attention_one_open_per_task_kind
           ON delegation_attention_requests(task_id, kind)
           WHERE status = 'open'"#,
    ] {
        conn.execute_unprepared(statement).await?;
    }
    fail_attention_at(
        failpoint,
        RebuildFailpoint::Index,
        "index",
        &source_table_sql,
        &source_indexes,
    )?;

    conn.execute_unprepared(
        r#"CREATE TABLE delegation_workflow_outbox_events (
           event_id TEXT PRIMARY KEY NOT NULL,
           workflow_id TEXT NOT NULL,
           graph_revision INTEGER NOT NULL,
           event_kind TEXT NOT NULL,
           subject_key TEXT NOT NULL,
           payload_json TEXT NOT NULL,
           dispatch_attempts INTEGER NOT NULL DEFAULT 0,
           created_at TEXT NOT NULL,
           delivered_at TEXT NULL,
           UNIQUE(workflow_id, graph_revision, event_kind, subject_key),
           FOREIGN KEY(workflow_id)
             REFERENCES delegation_workflows(workflow_id) ON DELETE CASCADE
         )"#,
    )
    .await?;

    verify_foreign_keys(conn).await?;
    fail_attention_at(
        failpoint,
        RebuildFailpoint::ForeignKeyCheck,
        "foreign_key_check",
        &source_table_sql,
        &source_indexes,
    )?;

    Ok(())
}

pub(super) async fn verify_foreign_keys<C: ConnectionTrait>(conn: &C) -> Result<(), DbErr> {
    let violations = conn
        .query_all(Statement::from_string(
            DbBackend::Sqlite,
            "PRAGMA foreign_key_check".to_owned(),
        ))
        .await?;
    if violations.is_empty() {
        Ok(())
    } else {
        Err(DbErr::Custom(format!(
            "completion migration foreign_key_check returned {} violation(s)",
            violations.len()
        )))
    }
}

async fn load_table_sql<C: ConnectionTrait>(conn: &C, table: &str) -> Result<String, DbErr> {
    conn.query_one(Statement::from_string(
        DbBackend::Sqlite,
        format!("SELECT sql FROM sqlite_master WHERE type = 'table' AND name = '{table}'"),
    ))
    .await?
    .ok_or_else(|| DbErr::Custom(format!("missing source table {table}")))?
    .try_get("", "sql")
}

async fn load_index_sql<C: ConnectionTrait>(
    conn: &C,
    table: &str,
) -> Result<Vec<(String, String)>, DbErr> {
    conn.query_all(Statement::from_string(
        DbBackend::Sqlite,
        format!(
            "SELECT name, sql FROM sqlite_master \
             WHERE type = 'index' AND tbl_name = '{table}' AND sql IS NOT NULL \
             ORDER BY name"
        ),
    ))
    .await?
    .into_iter()
    .map(|row| Ok((row.try_get("", "name")?, row.try_get("", "sql")?)))
    .collect()
}

async fn load_column_names<C: ConnectionTrait>(
    conn: &C,
    table: &str,
) -> Result<HashSet<String>, DbErr> {
    conn.query_all(Statement::from_string(
        DbBackend::Sqlite,
        format!("PRAGMA table_info({table})"),
    ))
    .await?
    .into_iter()
    .map(|row| row.try_get("", "name"))
    .collect()
}

fn source_value_expression(column: &str, source_columns: &HashSet<String>) -> String {
    if source_columns.contains(column) {
        return column.to_owned();
    }
    match column {
        "structural_revision" => "manifest_revision".to_owned(),
        "content_fingerprint" => "''".to_owned(),
        "stagnation_count" | "rewrite_used" => "0".to_owned(),
        _ => "NULL".to_owned(),
    }
}

async fn verify_projection<C: ConnectionTrait>(
    conn: &C,
    source_columns: &HashSet<String>,
    source_table_sql: &str,
    source_indexes: &[(String, String)],
) -> Result<(), DbErr> {
    let source_count = row_count(conn, SOURCE_TABLE).await?;
    let destination_count = row_count(conn, REPLACEMENT_TABLE).await?;
    if source_count != destination_count {
        return Err(rebuild_error(
            "row_count",
            source_table_sql,
            source_indexes,
            format!("source={source_count}, destination={destination_count}"),
        ));
    }

    let source_projection = V1_SETTLEMENT_COLUMNS
        .iter()
        .map(|column| encoded_value(&source_value_expression(column, source_columns)))
        .collect::<Vec<_>>()
        .join(", ");
    let destination_projection = V1_SETTLEMENT_COLUMNS
        .iter()
        .map(|column| encoded_value(column))
        .collect::<Vec<_>>()
        .join(", ");
    let source_only = difference_count(
        conn,
        SOURCE_TABLE,
        &source_projection,
        REPLACEMENT_TABLE,
        &destination_projection,
    )
    .await?;
    let destination_only = difference_count(
        conn,
        REPLACEMENT_TABLE,
        &destination_projection,
        SOURCE_TABLE,
        &source_projection,
    )
    .await?;
    if source_only != 0 || destination_only != 0 {
        return Err(rebuild_error(
            "byte_projection",
            source_table_sql,
            source_indexes,
            format!("source_only={source_only}, destination_only={destination_only}"),
        ));
    }

    Ok(())
}

async fn verify_attention_projection<C: ConnectionTrait>(
    conn: &C,
    source_table_sql: &str,
    source_indexes: &[(String, String)],
) -> Result<(), DbErr> {
    let source_count = row_count(conn, ATTENTION_SOURCE_TABLE).await?;
    let destination_count = row_count(conn, ATTENTION_REPLACEMENT_TABLE).await?;
    if source_count != destination_count {
        return Err(attention_rebuild_error(
            "row_count",
            source_table_sql,
            source_indexes,
            format!("source={source_count}, destination={destination_count}"),
        ));
    }

    let projection = V1_ATTENTION_COLUMNS
        .iter()
        .map(|column| encoded_value(column))
        .collect::<Vec<_>>()
        .join(", ");
    let source_only = difference_count(
        conn,
        ATTENTION_SOURCE_TABLE,
        &projection,
        ATTENTION_REPLACEMENT_TABLE,
        &projection,
    )
    .await?;
    let destination_only = difference_count(
        conn,
        ATTENTION_REPLACEMENT_TABLE,
        &projection,
        ATTENTION_SOURCE_TABLE,
        &projection,
    )
    .await?;
    if source_only != 0 || destination_only != 0 {
        return Err(attention_rebuild_error(
            "byte_projection",
            source_table_sql,
            source_indexes,
            format!("source_only={source_only}, destination_only={destination_only}"),
        ));
    }

    Ok(())
}

fn encoded_value(expression: &str) -> String {
    format!(
        "CASE WHEN ({expression}) IS NULL THEN 'null' \
         ELSE typeof(({expression})) || ':' || hex(CAST(({expression}) AS BLOB)) END"
    )
}

async fn row_count<C: ConnectionTrait>(conn: &C, table: &str) -> Result<i64, DbErr> {
    conn.query_one(Statement::from_string(
        DbBackend::Sqlite,
        format!("SELECT COUNT(*) AS row_count FROM {table}"),
    ))
    .await?
    .ok_or_else(|| DbErr::Custom(format!("row count returned no row for {table}")))?
    .try_get("", "row_count")
}

async fn difference_count<C: ConnectionTrait>(
    conn: &C,
    left_table: &str,
    left_projection: &str,
    right_table: &str,
    right_projection: &str,
) -> Result<i64, DbErr> {
    conn.query_one(Statement::from_string(
        DbBackend::Sqlite,
        format!(
            "SELECT COUNT(*) AS row_count FROM (\
               SELECT {left_projection} FROM {left_table} \
               EXCEPT SELECT {right_projection} FROM {right_table}\
             )"
        ),
    ))
    .await?
    .ok_or_else(|| DbErr::Custom("projection comparison returned no row".to_owned()))?
    .try_get("", "row_count")
}

fn fail_at(
    actual: RebuildFailpoint,
    expected: RebuildFailpoint,
    stage: &str,
    source_table_sql: &str,
    source_indexes: &[(String, String)],
) -> Result<(), DbErr> {
    if actual == expected {
        return Err(rebuild_error(
            stage,
            source_table_sql,
            source_indexes,
            "injected failure".to_owned(),
        ));
    }
    Ok(())
}

fn fail_attention_at(
    actual: RebuildFailpoint,
    expected: RebuildFailpoint,
    stage: &str,
    source_table_sql: &str,
    source_indexes: &[(String, String)],
) -> Result<(), DbErr> {
    if actual == expected {
        return Err(attention_rebuild_error(
            stage,
            source_table_sql,
            source_indexes,
            "injected failure".to_owned(),
        ));
    }
    Ok(())
}

fn rebuild_error(
    stage: &str,
    source_table_sql: &str,
    source_indexes: &[(String, String)],
    detail: String,
) -> DbErr {
    let index_names = source_indexes
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(",");
    DbErr::Custom(format!(
        "settlement rebuild failed at {stage}: {detail}; \
         source_table_sql={source_table_sql}; source_indexes={index_names}"
    ))
}

fn attention_rebuild_error(
    stage: &str,
    source_table_sql: &str,
    source_indexes: &[(String, String)],
    detail: String,
) -> DbErr {
    let index_names = source_indexes
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(",");
    DbErr::Custom(format!(
        "attention rebuild failed at {stage}: {detail}; \
         source_table_sql={source_table_sql}; source_indexes={index_names}"
    ))
}
