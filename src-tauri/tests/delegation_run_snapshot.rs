//! Task 7 snapshot DTO contract: one immutable parent-card view per run.

use codeg_lib::acp::delegation::card_summary::CardSummary;
use codeg_lib::commands::delegation::get_delegation_run_snapshot_core;
use codeg_lib::db::test_helpers::fresh_in_memory_db;
use sea_orm::{ConnectionTrait, DbBackend, Statement};

fn sql(text: impl Into<String>) -> Statement {
    Statement::from_string(DbBackend::Sqlite, text.into())
}

async fn seed_parent_child_and_runs(db: &codeg_lib::db::AppDatabase) {
    db.conn
        .execute(sql(
            "INSERT INTO folder \
             (id,name,path,last_opened_at,created_at,updated_at,is_open,sort_order,color,kind) \
             VALUES (1,'repo','/tmp/snapshot','2026-07-21','2026-07-21','2026-07-21',1,1,'inherit','regular')",
        ))
        .await
        .unwrap();
    for (id, kind, parent_id) in [(10, "regular", "NULL"), (20, "delegate", "10")] {
        db.conn
            .execute(sql(format!(
                "INSERT INTO conversation \
                 (id,folder_id,agent_type,status,kind,message_count,title_locked,auto_title_finalized,created_at,updated_at,parent_id) \
                 VALUES ({id},1,'codex','completed','{kind}',0,0,0,'2026-07-21','2026-07-21',{parent_id})"
            )))
            .await
            .unwrap();
    }
    for (task_id, generation, previous_task_id, summary) in [
        (
            "run-1",
            1,
            "NULL",
            "'{\"kind\":\"review\",\"verdict\":\"approve\",\"critical\":0,\"important\":0,\"minor\":1,\"summary\":\"First run\"}'",
        ),
        (
            "run-2",
            2,
            "'run-1'",
            "'{\"kind\":\"review\",\"verdict\":\"not_a_verdict\"}'",
        ),
    ] {
        db.conn
            .execute(sql(format!(
                "INSERT INTO delegation_task_runs \
                 (task_id,root_task_id,previous_task_id,generation,parent_conversation_id,parent_tool_use_id,child_conversation_id,agent_type,admission_class,lineage_root_task_id,history_only,status,started_at,finished_at,tool_call_count,edit_tool_call_count,touched_files_json,touched_files_truncated,additions,deletions,line_counts_complete,card_summary_json,created_at,updated_at) \
                 VALUES ('{task_id}','run-1',{previous_task_id},{generation},10,'tool-{task_id}',20,'codex','normal_revision','run-1',0,'completed','2026-07-21T10:00:00Z','2026-07-21T10:01:00Z',3,1,'[]',0,2,1,1,{summary},'2026-07-21T10:00:00Z','2026-07-21T10:01:00Z')"
            )))
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn snapshot_is_parent_scoped_and_uses_the_requested_run_not_child_projection() {
    let db = fresh_in_memory_db().await;
    seed_parent_child_and_runs(&db).await;

    // The child conversation is intentionally updated to its later run's
    // projection. A snapshot for run-1 must remain bound to run-1.
    db.conn
        .execute(sql(
            "UPDATE conversation SET delegation_call_id='run-2', delegation_task_status='completed', delegation_run_generation=2 WHERE id=20",
        ))
        .await
        .unwrap();

    let first = get_delegation_run_snapshot_core(&db.conn, 10, "run-1")
        .await
        .expect("owned run snapshot");
    assert_eq!(first.task_id, "run-1");
    assert_eq!(first.previous_task_id, None);
    assert_eq!(first.generation, 1);
    assert_eq!(first.child_conversation_id, 20);
    assert_eq!(
        first
            .runtime_stats
            .as_ref()
            .map(|stats| stats.tool_call_count),
        Some(3)
    );
    assert!(matches!(
        first.card_summary,
        Some(CardSummary::Review { ref summary, .. }) if summary == "First run"
    ));

    let second = get_delegation_run_snapshot_core(&db.conn, 10, "run-2")
        .await
        .expect("later owned run snapshot");
    assert_eq!(second.task_id, "run-2");
    assert_eq!(second.previous_task_id.as_deref(), Some("run-1"));
    assert_eq!(second.generation, 2);
    assert!(second.card_summary.is_none());

    let err = get_delegation_run_snapshot_core(&db.conn, 999, "run-1")
        .await
        .expect_err("a different parent cannot read another run");
    assert_eq!(err.to_string(), "delegation run not found");
}
