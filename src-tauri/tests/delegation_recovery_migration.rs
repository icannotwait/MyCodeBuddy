//! Migration coverage for shared delegation/workflow recovery persistence.

use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement};
use sea_orm_migration::MigratorTrait;

use codeg_lib::db::migration::Migrator;

const EXISTING_MIGRATION_COUNT: u32 = 42;

fn sql(text: impl Into<String>) -> Statement {
    Statement::from_string(DbBackend::Sqlite, text.into())
}

async fn open_db() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:")
        .await
        .expect("database");
    db.execute(sql("PRAGMA foreign_keys=ON;"))
        .await
        .expect("enable foreign keys");
    db
}

async fn migrate_existing_schema(db: &DatabaseConnection) {
    Migrator::up(db, Some(EXISTING_MIGRATION_COUNT))
        .await
        .expect("migrate through existing 42 migrations");
}

async fn apply_recovery_migration(db: &DatabaseConnection) {
    Migrator::up(db, None)
        .await
        .expect("apply recovery migration 43");
}

async fn seed_historical_state(db: &DatabaseConnection) {
    db.execute(sql("INSERT INTO folder \
         (id,name,path,last_opened_at,created_at,updated_at,is_open,sort_order,color,kind) \
         VALUES (1,'repo','C:/recovery-fixture','2026-07-29','2026-07-29',\
                 '2026-07-29',1,1,'inherit','regular')"))
        .await
        .expect("seed folder");

    db.execute(sql("INSERT INTO conversation \
         (id,folder_id,agent_type,status,kind,message_count,title_locked,auto_title_finalized,\
          parent_id,delegation_call_id,delegation_task_status,delegation_error_code,\
          delegation_started_at,delegation_finished_at,delegation_run_generation,\
          created_at,updated_at) VALUES \
         (1,1,'codex','completed','regular',0,0,0,NULL,NULL,NULL,NULL,NULL,NULL,NULL,\
          '2026-07-29T00:00:00Z','2026-07-29T00:00:00Z'),\
         (2,1,'codex','completed','delegate',1,0,0,1,'call-disconnect','failed',\
          'parent_disconnect','2026-07-29T00:01:00Z','2026-07-29T00:02:00Z',1,\
          '2026-07-29T00:01:00Z','2026-07-29T00:02:00Z'),\
         (3,1,'codex','completed','delegate',0,0,0,1,'call-abort','canceled',\
          'pure_abort','2026-07-29T00:03:00Z','2026-07-29T00:04:00Z',1,\
          '2026-07-29T00:03:00Z','2026-07-29T00:04:00Z'),\
         (4,1,'codex','completed','delegate',0,0,0,1,'call-admission','failed',\
          'admission_unknown','2026-07-29T00:05:00Z','2026-07-29T00:06:00Z',1,\
          '2026-07-29T00:05:00Z','2026-07-29T00:06:00Z'),\
         (5,1,'codex','completed','delegate',2,0,0,1,'call-replacement','completed',\
          NULL,'2026-07-29T00:07:00Z','2026-07-29T00:10:00Z',2,\
          '2026-07-29T00:07:00Z','2026-07-29T00:10:00Z')"))
        .await
        .expect("seed parent and delegation children");

    for statement in [
        r#"INSERT INTO delegation_task_runs (
             task_id,root_task_id,previous_task_id,generation,parent_conversation_id,
             child_conversation_id,agent_type,admission_class,reached_running_at,
             lineage_root_task_id,work_unit_key,history_only,status,error_code,
             termination_audit_json,started_at,finished_at,tool_call_count,
             edit_tool_call_count,touched_files_json,touched_files_truncated,
             additions,deletions,line_counts_complete,card_summary_json,
             child_turn_anchor,child_connection_id,replaced_task_id,replacement_reason,
             created_at,updated_at
           ) VALUES (
             'task-parent-disconnect','task-parent-disconnect',NULL,1,1,
             2,'codex','normal_revision','2026-07-29T00:01:01Z',
             'task-parent-disconnect','unit-disconnect',0,'failed','parent_disconnect',
             NULL,'2026-07-29T00:01:00Z','2026-07-29T00:02:00Z',3,
             1,'["src/a.rs"]',0,12,4,1,'{"summary":"disconnect"}',
             'turn-a','connection-a',NULL,NULL,
             '2026-07-29T00:01:00Z','2026-07-29T00:02:00Z'
           )"#,
        r#"INSERT INTO delegation_task_runs (
             task_id,root_task_id,previous_task_id,generation,parent_conversation_id,
             child_conversation_id,agent_type,admission_class,lineage_root_task_id,
             work_unit_key,history_only,status,error_code,termination_audit_json,
             started_at,finished_at,created_at,updated_at
           ) VALUES (
             'task-pure-abort','task-pure-abort',NULL,1,1,
             3,'codex','normal_revision','task-pure-abort',
             'unit-abort',0,'canceled','pure_abort','{"kind":"abort", "bytes":"kept  exactly"}',
             '2026-07-29T00:03:00Z','2026-07-29T00:04:00Z',
             '2026-07-29T00:03:00Z','2026-07-29T00:04:00Z'
           )"#,
        r#"INSERT INTO delegation_task_runs (
             task_id,root_task_id,previous_task_id,generation,parent_conversation_id,
             child_conversation_id,agent_type,admission_class,lineage_root_task_id,
             work_unit_key,history_only,status,error_code,termination_audit_json,
             started_at,finished_at,created_at,updated_at
           ) VALUES (
             'task-admission-unknown','task-admission-unknown',NULL,1,1,
             4,'codex','normal_revision','task-admission-unknown',
             'unit-admission',0,'failed','admission_unknown','{"admission":"unknown"}',
             '2026-07-29T00:05:00Z','2026-07-29T00:06:00Z',
             '2026-07-29T00:05:00Z','2026-07-29T00:06:00Z'
           )"#,
        r#"INSERT INTO delegation_task_runs (
             task_id,root_task_id,previous_task_id,generation,parent_conversation_id,
             child_conversation_id,agent_type,admission_class,reached_running_at,
             lineage_root_task_id,work_unit_key,history_only,status,error_code,
             termination_audit_json,started_at,finished_at,created_at,updated_at
           ) VALUES (
             'task-original','task-original',NULL,1,1,
             5,'codex','normal_revision','2026-07-29T00:07:01Z',
             'task-original','task|1|implementer|codex|none',0,'failed','parent_disconnect',
             NULL,'2026-07-29T00:07:00Z','2026-07-29T00:08:00Z',
             '2026-07-29T00:07:00Z','2026-07-29T00:08:00Z'
           )"#,
        r#"INSERT INTO delegation_task_runs (
             task_id,root_task_id,previous_task_id,generation,parent_conversation_id,
             child_conversation_id,agent_type,admission_class,reached_running_at,
             lineage_root_task_id,work_unit_key,history_only,status,error_code,
             termination_audit_json,started_at,finished_at,replaced_task_id,
             replacement_reason,created_at,updated_at
           ) VALUES (
             'task-replacement','task-original','task-original',2,1,
             5,'codex','replacement','2026-07-29T00:09:01Z',
             'task-original','task|1|implementer|codex|none',0,'completed',NULL,
             '{"result":"replacement complete"}','2026-07-29T00:09:00Z',
             '2026-07-29T00:10:00Z','task-original','terminal_replacement',
             '2026-07-29T00:09:00Z','2026-07-29T00:10:00Z'
           )"#,
        "INSERT INTO delegation_lineage_budgets \
         (lineage_root_task_id,unexpected_continue_count,replacement_count) \
         VALUES ('task-original',2,3)",
        "INSERT INTO delegation_work_unit_budgets \
         (parent_conversation_id,work_unit_key,unexpected_continue_count,replacement_count) \
         VALUES (1,'task|1|implementer|codex|none',4,5)",
    ] {
        db.execute(sql(statement)).await.expect("seed run history");
    }

    for statement in [
        r#"INSERT INTO delegation_workflows (
             workflow_id,parent_conversation_id,workflow_kind,schema_version,
             active_manifest_revision,graph_revision,workflow_state,capability_version,
             publication_token,supersedes_approved_revision,structural_revision,
             design_fingerprint,plan_fingerprint,created_at,updated_at
           ) VALUES (
             'workflow-recovery',1,'brainstorm_to_delivery',2,
             8,17,'blocked','workflow_manifest_v2',
             'publication-revision-8',7,7,
             'design-fingerprint-v7','plan-fingerprint-v8',
             '2026-07-29T01:00:00Z','2026-07-29T02:00:00Z'
           )"#,
        r#"INSERT INTO delegation_workflow_manifest_revisions (
             workflow_id,manifest_revision,manifest_state,document_json,
             document_digest,created_at
           ) VALUES (
             'workflow-recovery',8,'blocked',
             '{"workflow_state":"blocked", "tasks":["one", "two"]}',
             'digest-revision-8','2026-07-29T02:00:00Z'
           )"#,
        r#"INSERT INTO delegation_workflow_node_bindings (
             workflow_id,node_id,work_unit_key,role,agent_type,phase_id,task_index,
             introduced_revision,retired_revision,is_observed,retained_observed,
             cohort_frozen,node_outcome,created_at,updated_at
           ) VALUES (
             'workflow-recovery','retired-observer','unit-retired','reviewer','codex',
             'tasks',1,3,7,1,1,0,NULL,
             '2026-07-29T01:10:00Z','2026-07-29T01:50:00Z'
           )"#,
        r#"INSERT INTO delegation_workflow_node_bindings (
             workflow_id,node_id,work_unit_key,role,agent_type,phase_id,task_index,
             introduced_revision,retired_revision,is_observed,retained_observed,
             cohort_frozen,node_outcome,created_at,updated_at
           ) VALUES (
             'workflow-recovery','frozen-implementer','unit-frozen','implementer','codex',
             'tasks',2,6,NULL,0,0,1,NULL,
             '2026-07-29T01:20:00Z','2026-07-29T02:00:00Z'
           )"#,
        r#"INSERT INTO delegation_workflow_gate_settlements (
             workflow_id,gate_id,gate_cycle,manifest_revision,structural_revision,
             content_fingerprint,outcome,critical_count,important_count,minor_count,
             summary,graph_revision_at_settle,review_scope,revision_kind,scope_reason,
             required_reviewer_node_ids_json,covered_author_task_id,covered_plan_digest,
             finding_ledger_json,net_improvement,stagnation_count,rewrite_used,next_action,
             report_files_json,created_at
           ) VALUES (
             'workflow-recovery','plan',1,7,7,
             'plan-fingerprint-v7','changes_requested',0,2,1,
             'user decision required',15,'full','material','scope changed',
             '["reviewer-a","reviewer-b"]','task-plan-author','plan-digest-v7',
             '{"findings":["important"]}',0,2,1,'user_decision_required',
             '["reports/plan-cycle-1.md"]','2026-07-29T01:40:00Z'
           )"#,
        r#"INSERT INTO delegation_workflow_gate_settlements (
             workflow_id,gate_id,gate_cycle,manifest_revision,structural_revision,
             content_fingerprint,outcome,critical_count,important_count,minor_count,
             summary,graph_revision_at_settle,review_scope,revision_kind,scope_reason,
             required_reviewer_node_ids_json,covered_author_task_id,covered_plan_digest,
             finding_ledger_json,net_improvement,stagnation_count,rewrite_used,next_action,
             report_files_json,created_at
           ) VALUES (
             'workflow-recovery','plan',2,8,7,
             'plan-fingerprint-v8','approved',0,0,0,
             'approved current Plan gate',17,'scoped','localized','final scoped review',
             '["reviewer-b"]','task-plan-author-v8','plan-digest-v8',
             '{"findings":[]}',1,0,1,'approved',
             '["reports/plan-cycle-2.md"]','2026-07-29T02:00:00Z'
           )"#,
    ] {
        db.execute(sql(statement))
            .await
            .expect("seed workflow history");
    }
}

async fn selected_historical_bytes(db: &DatabaseConnection) -> Vec<String> {
    let queries = [
        r#"SELECT 'conversation|' || id || '|' || quote(status) || '|' || quote(kind) ||
                  '|' || quote(delegation_task_status) || '|' || quote(delegation_error_code) ||
                  '|' || quote(delegation_started_at) || '|' || quote(delegation_finished_at) ||
                  '|' || quote(delegation_run_generation) AS snapshot
           FROM conversation WHERE id BETWEEN 1 AND 5 ORDER BY id"#,
        r#"SELECT 'run|' || task_id || '|' || quote(root_task_id) || '|' ||
                  quote(previous_task_id) || '|' || quote(generation) || '|' ||
                  quote(admission_class) || '|' || quote(reached_running_at) || '|' ||
                  quote(lineage_root_task_id) || '|' || quote(work_unit_key) || '|' ||
                  quote(history_only) || '|' || quote(status) || '|' || quote(error_code) ||
                  '|' || quote(termination_audit_json) || '|' || quote(touched_files_json) ||
                  '|' || quote(card_summary_json) || '|' || quote(replaced_task_id) ||
                  '|' || quote(replacement_reason) AS snapshot
           FROM delegation_task_runs ORDER BY task_id"#,
        r#"SELECT 'lineage-budget|' || lineage_root_task_id || '|' ||
                  unexpected_continue_count || '|' || replacement_count AS snapshot
           FROM delegation_lineage_budgets ORDER BY lineage_root_task_id"#,
        r#"SELECT 'work-unit-budget|' || parent_conversation_id || '|' ||
                  quote(work_unit_key) || '|' || unexpected_continue_count || '|' ||
                  replacement_count AS snapshot
           FROM delegation_work_unit_budgets ORDER BY parent_conversation_id, work_unit_key"#,
        r#"SELECT 'workflow|' || workflow_id || '|' || parent_conversation_id || '|' ||
                  quote(workflow_kind) || '|' || schema_version || '|' ||
                  active_manifest_revision || '|' || graph_revision || '|' ||
                  quote(workflow_state) || '|' || quote(capability_version) || '|' ||
                  quote(publication_token) || '|' || quote(supersedes_approved_revision) || '|' ||
                  structural_revision || '|' || quote(design_fingerprint) || '|' ||
                  quote(plan_fingerprint) AS snapshot
           FROM delegation_workflows ORDER BY workflow_id"#,
        r#"SELECT 'manifest|' || workflow_id || '|' || manifest_revision || '|' ||
                  quote(manifest_state) || '|' || hex(CAST(document_json AS BLOB)) || '|' ||
                  quote(document_digest) || '|' || quote(created_at) AS snapshot
           FROM delegation_workflow_manifest_revisions
           ORDER BY workflow_id, manifest_revision"#,
        r#"SELECT 'binding|' || workflow_id || '|' || node_id || '|' ||
                  quote(work_unit_key) || '|' || quote(role) || '|' || quote(agent_type) ||
                  '|' || quote(phase_id) || '|' || quote(task_index) || '|' ||
                  introduced_revision || '|' || quote(retired_revision) || '|' ||
                  is_observed || '|' || retained_observed || '|' || cohort_frozen || '|' ||
                  quote(node_outcome) AS snapshot
           FROM delegation_workflow_node_bindings ORDER BY workflow_id, node_id"#,
        r#"SELECT 'settlement|' || workflow_id || '|' || gate_id || '|' || gate_cycle ||
                  '|' || manifest_revision || '|' || structural_revision || '|' ||
                  quote(content_fingerprint) || '|' || quote(outcome) || '|' ||
                  critical_count || '|' || important_count || '|' || minor_count || '|' ||
                  quote(summary) || '|' || graph_revision_at_settle || '|' ||
                  quote(review_scope) || '|' || quote(revision_kind) || '|' ||
                  quote(scope_reason) || '|' || quote(required_reviewer_node_ids_json) || '|' ||
                  quote(covered_author_task_id) || '|' || quote(covered_plan_digest) || '|' ||
                  quote(finding_ledger_json) || '|' || quote(net_improvement) || '|' ||
                  stagnation_count || '|' || rewrite_used || '|' || quote(next_action) || '|' ||
                  quote(report_files_json) AS snapshot
           FROM delegation_workflow_gate_settlements
           ORDER BY workflow_id, gate_id, gate_cycle"#,
    ];

    let mut snapshots = Vec::new();
    for query in queries {
        snapshots.extend(
            db.query_all(sql(query))
                .await
                .expect("load historical snapshot")
                .into_iter()
                .map(|row| row.try_get::<String>("", "snapshot").expect("snapshot")),
        );
    }
    snapshots
}

async fn insert_authorization(
    db: &DatabaseConnection,
    id: &str,
    subject_kind: &str,
    subject_id: &str,
    fingerprint: &str,
    status: &str,
    consumed: bool,
) -> Result<sea_orm::ExecResult, sea_orm::DbErr> {
    let consumer_values = if consumed {
        ",'2026-07-29T03:05:00Z','delegation_task_run','task-replacement','correlation-1'"
    } else {
        ",NULL,NULL,NULL,NULL"
    };
    db.execute(sql(format!(
        "INSERT INTO recovery_authorizations (\
           authorization_id,parent_conversation_id,subject_kind,subject_id,\
           source_state_fingerprint,allowed_action,action_payload_json,cause_code,\
           risk_class,status,requested_at,consumed_at,consumed_by_kind,consumed_by_id,\
           consumer_correlation_id\
         ) VALUES (\
           '{id}',1,'{subject_kind}','{subject_id}','{fingerprint}',\
           'retry','{{\"mode\":\"exact\"}}','parent_disconnect','elevated',\
           '{status}','2026-07-29T03:00:00Z'{consumer_values}\
         )"
    )))
    .await
}

#[tokio::test]
async fn recovery_migration_preserves_existing_workflow_and_run_bytes() {
    let db = open_db().await;
    migrate_existing_schema(&db).await;
    seed_historical_state(&db).await;
    let before = selected_historical_bytes(&db).await;

    apply_recovery_migration(&db).await;

    let run_columns = db
        .query_all(sql("PRAGMA table_info(delegation_task_runs)"))
        .await
        .expect("task run columns");
    assert!(
        run_columns.iter().any(|row| {
            row.try_get::<String>("", "name").expect("column name") == "recovery_authorization_id"
        }),
        "migration 43 must add delegation_task_runs.recovery_authorization_id"
    );
    let manifest_columns = db
        .query_all(sql(
            "PRAGMA table_info(delegation_workflow_manifest_revisions)",
        ))
        .await
        .expect("manifest revision columns");
    assert!(
        manifest_columns.iter().any(|row| {
            row.try_get::<String>("", "name").expect("column name") == "graph_revision"
        }),
        "migration 43 must add immutable manifest graph_revision evidence"
    );
    let historical_graph_revision = db
        .query_one(sql(
            "SELECT graph_revision FROM delegation_workflow_manifest_revisions \
             WHERE workflow_id='workflow-recovery' AND manifest_revision=8",
        ))
        .await
        .expect("historical manifest graph revision")
        .expect("historical manifest row")
        .try_get::<Option<i64>>("", "graph_revision")
        .expect("nullable graph revision");
    assert_eq!(historical_graph_revision, None);
    assert_eq!(selected_historical_bytes(&db).await, before);
}

#[tokio::test]
async fn recovery_migration_adds_one_active_challenge_and_provenance_columns() {
    let db = open_db().await;
    migrate_existing_schema(&db).await;
    seed_historical_state(&db).await;
    apply_recovery_migration(&db).await;

    insert_authorization(
        &db,
        "auth-pending",
        "delegation_task",
        "task-parent-disconnect",
        "fingerprint-1",
        "pending",
        false,
    )
    .await
    .expect("insert pending challenge");

    let competing = insert_authorization(
        &db,
        "auth-approved",
        "delegation_task",
        "task-parent-disconnect",
        "fingerprint-1",
        "approved",
        false,
    )
    .await
    .expect_err("active challenge index accepted competing approval");
    assert!(
        competing.to_string().contains("UNIQUE constraint failed"),
        "competing challenge failed for the wrong reason: {competing}"
    );

    insert_authorization(
        &db,
        "auth-declined",
        "delegation_task",
        "task-parent-disconnect",
        "fingerprint-1",
        "declined",
        false,
    )
    .await
    .expect("declined history may share a fingerprint");
    insert_authorization(
        &db,
        "auth-consumed",
        "delegation_task",
        "task-parent-disconnect",
        "fingerprint-1",
        "consumed",
        true,
    )
    .await
    .expect("consumed history may share a fingerprint");

    let indexes = db
        .query_all(sql("SELECT name FROM sqlite_master \
             WHERE type='index' AND tbl_name='recovery_authorizations' \
               AND name LIKE 'idx_ra_%' ORDER BY name"))
        .await
        .expect("authorization indexes")
        .into_iter()
        .map(|row| row.try_get::<String>("", "name").expect("index name"))
        .collect::<Vec<_>>();
    assert_eq!(
        indexes,
        vec![
            "idx_ra_consumed_by",
            "idx_ra_one_active_challenge",
            "idx_ra_parent_status",
            "idx_ra_question_id",
            "idx_ra_status_expires_at",
        ]
    );

    let index_sql = db
        .query_one(sql("SELECT sql FROM sqlite_master \
             WHERE type='index' AND name='idx_ra_one_active_challenge'"))
        .await
        .expect("active index query")
        .expect("active index row")
        .try_get::<String>("", "sql")
        .expect("active index SQL");
    let normalized_index_sql = index_sql
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>();
    assert!(
        normalized_index_sql
            .contains("(parent_conversation_id,subject_kind,subject_id,source_state_fingerprint)"),
        "wrong active index columns: {normalized_index_sql}"
    );
    assert!(
        normalized_index_sql.contains("WHEREstatusIN('pending','approved')"),
        "wrong active index predicate: {normalized_index_sql}"
    );

    let pending = db
        .query_one(sql(
            "SELECT source_task_id,child_conversation_id,lineage_root_task_id,work_unit_key,\
                    display_reason,question_id,approved_at,expires_at,consumed_at,\
                    consumed_by_kind,consumed_by_id,consumer_correlation_id \
             FROM recovery_authorizations WHERE authorization_id='auth-pending'",
        ))
        .await
        .expect("pending authorization")
        .expect("pending row");
    for column in [
        "source_task_id",
        "lineage_root_task_id",
        "work_unit_key",
        "display_reason",
        "question_id",
        "approved_at",
        "expires_at",
        "consumed_at",
        "consumed_by_kind",
        "consumed_by_id",
        "consumer_correlation_id",
    ] {
        assert_eq!(pending.try_get::<Option<String>>("", column).unwrap(), None);
    }
    assert_eq!(
        pending
            .try_get::<Option<i64>>("", "child_conversation_id")
            .unwrap(),
        None
    );

    let conversation = db
        .query_one(sql(
            "SELECT last_termination_audit_json FROM conversation WHERE id=2",
        ))
        .await
        .expect("conversation provenance")
        .expect("conversation row");
    assert_eq!(
        conversation
            .try_get::<Option<String>>("", "last_termination_audit_json")
            .unwrap(),
        None
    );
    let run = db
        .query_one(sql(
            "SELECT recovery_authorization_id FROM delegation_task_runs \
             WHERE task_id='task-parent-disconnect'",
        ))
        .await
        .expect("run provenance")
        .expect("run row");
    assert_eq!(
        run.try_get::<Option<String>>("", "recovery_authorization_id")
            .unwrap(),
        None
    );
    let workflow = db
        .query_one(sql(
            "SELECT block_cause_code,block_source_manifest_revision \
             FROM delegation_workflows WHERE workflow_id='workflow-recovery'",
        ))
        .await
        .expect("workflow provenance")
        .expect("workflow row");
    assert_eq!(
        workflow
            .try_get::<Option<String>>("", "block_cause_code")
            .unwrap(),
        None
    );
    assert_eq!(
        workflow
            .try_get::<Option<i64>>("", "block_source_manifest_revision")
            .unwrap(),
        None
    );
    let revision = db
        .query_one(sql(
            "SELECT revision_kind,source_manifest_revision,recovery_authorization_id,\
                    transition_reason_code,consumer_correlation_id \
             FROM delegation_workflow_manifest_revisions \
             WHERE workflow_id='workflow-recovery' AND manifest_revision=8",
        ))
        .await
        .expect("manifest provenance")
        .expect("manifest row");
    for column in [
        "revision_kind",
        "recovery_authorization_id",
        "transition_reason_code",
        "consumer_correlation_id",
    ] {
        assert_eq!(
            revision.try_get::<Option<String>>("", column).unwrap(),
            None
        );
    }
    assert_eq!(
        revision
            .try_get::<Option<i64>>("", "source_manifest_revision")
            .unwrap(),
        None
    );
    let settlement = db
        .query_one(sql(
            "SELECT lineage_reset_authorization_id \
             FROM delegation_workflow_gate_settlements \
             WHERE workflow_id='workflow-recovery' AND gate_id='plan' AND gate_cycle=2",
        ))
        .await
        .expect("settlement provenance")
        .expect("settlement row");
    assert_eq!(
        settlement
            .try_get::<Option<String>>("", "lineage_reset_authorization_id")
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn deleting_parent_conversation_removes_recovery_authorizations() {
    let db = open_db().await;
    migrate_existing_schema(&db).await;
    seed_historical_state(&db).await;
    apply_recovery_migration(&db).await;

    insert_authorization(
        &db,
        "auth-task",
        "delegation_task",
        "task-parent-disconnect",
        "fingerprint-task",
        "declined",
        false,
    )
    .await
    .expect("task authorization");
    insert_authorization(
        &db,
        "auth-workflow",
        "workflow",
        "workflow-recovery",
        "fingerprint-workflow",
        "consumed",
        true,
    )
    .await
    .expect("workflow authorization");

    db.execute(sql("DELETE FROM conversation WHERE id=1"))
        .await
        .expect("delete parent conversation");
    let count = db
        .query_one(sql("SELECT COUNT(*) AS count FROM recovery_authorizations"))
        .await
        .expect("authorization count")
        .expect("count row")
        .try_get::<i64>("", "count")
        .expect("count value");
    assert_eq!(count, 0);
}
