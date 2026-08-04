use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement};
use sea_orm_migration::{MigrationTrait, MigratorTrait};

use codeg_lib::db::migration::Migrator;

const MIGRATION_1: &str = "m20260804_000001_completion_protocol_and_run_evidence";
const MIGRATION_2: &str = "m20260804_000002_completion_scope_and_gate_settlement";

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

    // Task 1 ships before migration 2; once appended, it must be the successor.
    if let Some(next) = names.get(first + 1) {
        assert_eq!(*next, MIGRATION_2);
    }
}
