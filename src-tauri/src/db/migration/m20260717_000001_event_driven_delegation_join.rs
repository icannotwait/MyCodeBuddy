use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        for statement in [
            "ALTER TABLE conversation ADD COLUMN delegation_tool_call_count INTEGER NULL",
            "ALTER TABLE conversation ADD COLUMN delegation_edit_tool_call_count INTEGER NULL",
            "ALTER TABLE conversation ADD COLUMN delegation_touched_files_json TEXT NULL",
            "ALTER TABLE conversation ADD COLUMN delegation_touched_files_truncated BOOLEAN NULL",
            "ALTER TABLE conversation ADD COLUMN delegation_additions INTEGER NULL",
            "ALTER TABLE conversation ADD COLUMN delegation_deletions INTEGER NULL",
            "ALTER TABLE conversation ADD COLUMN delegation_line_counts_complete BOOLEAN NULL",
            "CREATE TABLE delegation_attention_requests (\
               request_id TEXT PRIMARY KEY NOT NULL,\
               task_id TEXT NOT NULL,\
               parent_conversation_id INTEGER NOT NULL,\
               child_conversation_id INTEGER NOT NULL,\
               child_tool_call_id TEXT NOT NULL,\
               status TEXT NOT NULL CHECK (status IN ('open','resolved')),\
               message TEXT NOT NULL,\
               reply TEXT NULL,\
               resolution_code TEXT NULL,\
               created_at TEXT NOT NULL,\
               resolved_at TEXT NULL,\
               FOREIGN KEY(parent_conversation_id) REFERENCES conversation(id) ON DELETE CASCADE,\
               FOREIGN KEY(child_conversation_id) REFERENCES conversation(id) ON DELETE CASCADE\
             )",
            "CREATE UNIQUE INDEX idx_attention_task_tool_call \
               ON delegation_attention_requests(task_id, child_tool_call_id)",
            "CREATE UNIQUE INDEX idx_attention_one_open_per_task \
               ON delegation_attention_requests(task_id) WHERE status='open'",
            "CREATE INDEX idx_attention_parent_status_created \
               ON delegation_attention_requests(parent_conversation_id, status, created_at)",
        ] {
            conn.execute_unprepared(statement).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared("DROP TABLE delegation_attention_requests")
            .await?;
        for column in [
            "delegation_line_counts_complete",
            "delegation_deletions",
            "delegation_additions",
            "delegation_touched_files_truncated",
            "delegation_touched_files_json",
            "delegation_edit_tool_call_count",
            "delegation_tool_call_count",
        ] {
            conn.execute_unprepared(&format!("ALTER TABLE conversation DROP COLUMN {column}"))
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
    use sea_orm_migration::MigratorTrait;

    use crate::db::migration::Migrator;

    fn sql(text: impl Into<String>) -> Statement {
        Statement::from_string(DbBackend::Sqlite, text.into())
    }

    #[tokio::test]
    async fn migration_keeps_historical_rollups_null_and_enforces_attention_uniqueness() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let migrations = <Migrator as MigratorTrait>::migrations();
        let target = migrations
            .iter()
            .position(|m| m.name().contains("event_driven_delegation_join"))
            .expect("join migration registered");
        Migrator::up(&db, Some(target as u32)).await.unwrap();
        db.execute(sql(
            "INSERT INTO folder (id,name,path,last_opened_at,created_at,updated_at,is_open,sort_order,color,kind) \
             VALUES (1,'repo','C:/repo','2026-07-17','2026-07-17','2026-07-17',1,1,'inherit','regular')",
        ))
        .await
        .unwrap();
        db.execute(sql(
            "INSERT INTO conversation \
             (id,folder_id,agent_type,status,kind,message_count,title_locked,auto_title_finalized,created_at,updated_at) \
             VALUES (1,1,'codex','completed','regular',0,0,0,'2026-07-17','2026-07-17')",
        ))
        .await
        .unwrap();
        Migrator::up(&db, None).await.unwrap();

        let row = db
            .query_one(sql(
                "SELECT delegation_tool_call_count AS tools, delegation_touched_files_json AS files \
                 FROM conversation WHERE id=1",
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.try_get::<Option<i64>>("", "tools").unwrap(), None);
        assert_eq!(row.try_get::<Option<String>>("", "files").unwrap(), None);

        db.execute(sql(
            "INSERT INTO conversation \
             (id,folder_id,agent_type,status,kind,message_count,title_locked,auto_title_finalized,created_at,updated_at, \
              parent_id,parent_tool_use_id,delegation_call_id,delegation_task_status,delegation_started_at) \
             VALUES (2,1,'codex','in_progress','delegate',0,0,0,'2026-07-17','2026-07-17', \
                     1,'tool-1','task-1','running','2026-07-17')",
        ))
        .await
        .unwrap();
        let insert = |request_id: &str, child_tool_call_id: &str| {
            sql(format!(
                "INSERT INTO delegation_attention_requests \
                 (request_id,task_id,parent_conversation_id,child_conversation_id,child_tool_call_id,status,message,created_at) \
                 VALUES ('{request_id}','task-1',1,2,'{child_tool_call_id}','open','choose','2026-07-17')"
            ))
        };
        db.execute(insert("r1", "tc-1")).await.unwrap();
        assert!(db.execute(insert("r2", "tc-2")).await.is_err());
        assert!(db.execute(insert("r3", "tc-1")).await.is_err());
    }
}
