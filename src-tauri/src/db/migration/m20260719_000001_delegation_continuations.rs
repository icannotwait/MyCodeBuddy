use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        for statement in [
            "CREATE TABLE delegation_continuations (\
               continuation_id TEXT PRIMARY KEY NOT NULL,\
               parent_conversation_id INTEGER NOT NULL,\
               parent_session_id TEXT NOT NULL,\
               parent_connection_id TEXT NULL,\
               generation INTEGER NOT NULL CHECK (generation > 0),\
               parent_turn_generation INTEGER NOT NULL CHECK (parent_turn_generation > 0),\
               task_ids_json TEXT NOT NULL,\
               state TEXT NOT NULL CHECK (state IN (\
                 'arming','waiting','wake_pending','resuming',\
                 'completed','cancelled','failed'\
               )),\
               wake_reason TEXT NULL CHECK (wake_reason IS NULL OR wake_reason IN (\
                 'all_terminal','attention_required','unavailable','checkpoint'\
               )),\
               armed_at TEXT NOT NULL,\
               wake_at TEXT NOT NULL,\
               suspend_requested_at TEXT NULL,\
               suspended_at TEXT NULL,\
               wake_claimed_at TEXT NULL,\
               prompt_admitted_at TEXT NULL,\
               finished_at TEXT NULL,\
               internal_prompt_id TEXT NOT NULL,\
               internal_prompt_marker TEXT NOT NULL,\
               failure_code TEXT NULL CHECK (failure_code IS NULL OR failure_code IN (\
                 'arm_failed','suspend_dispatch_failed','suspend_drain_timeout',\
                 'parent_connection_lost','prompt_delivery_failed','state_conflict'\
               )),\
               version INTEGER NOT NULL CHECK (version >= 0),\
               created_at TEXT NOT NULL,\
               updated_at TEXT NOT NULL,\
               FOREIGN KEY(parent_conversation_id)\
                 REFERENCES conversation(id) ON DELETE CASCADE\
             )",
            "CREATE UNIQUE INDEX idx_cont_parent_generation \
             ON delegation_continuations(parent_conversation_id, generation)",
            "CREATE UNIQUE INDEX idx_cont_internal_prompt_id \
             ON delegation_continuations(internal_prompt_id)",
            "CREATE UNIQUE INDEX idx_cont_internal_prompt_marker \
             ON delegation_continuations(internal_prompt_marker)",
            "CREATE UNIQUE INDEX idx_cont_one_active_per_parent \
             ON delegation_continuations(parent_conversation_id) \
             WHERE state IN ('arming','waiting','wake_pending','resuming')",
            "CREATE INDEX idx_cont_state_updated \
             ON delegation_continuations(state, updated_at)",
        ] {
            conn.execute_unprepared(statement).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE delegation_continuations")
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
    use sea_orm_migration::{MigrationTrait, MigratorTrait, SchemaManager};

    use crate::db::migration::{m20260719_000001_delegation_continuations::Migration, Migrator};

    fn sql(text: impl Into<String>) -> Statement {
        Statement::from_string(DbBackend::Sqlite, text.into())
    }

    async fn seed_parent_conversation(db: &sea_orm::DatabaseConnection) {
        db.execute(sql(
            "INSERT INTO folder \
             (id,name,path,last_opened_at,created_at,updated_at,is_open,sort_order,color,kind) \
             VALUES (1,'repo','C:/repo','2026-07-19','2026-07-19','2026-07-19',1,1,'inherit','regular')",
        ))
        .await
        .unwrap();
        db.execute(sql(
            "INSERT INTO conversation \
             (id,folder_id,agent_type,status,kind,message_count,title_locked,auto_title_finalized,created_at,updated_at) \
             VALUES (1,1,'codex','completed','regular',0,0,0,'2026-07-19','2026-07-19')",
        ))
        .await
        .unwrap();
    }

    fn continuation_insert(
        continuation_id: &str,
        generation: u64,
        state: &str,
        internal_prompt_id: &str,
        internal_prompt_marker: &str,
    ) -> Statement {
        sql(format!(
            "INSERT INTO delegation_continuations \
             (continuation_id,parent_conversation_id,parent_session_id,generation,parent_turn_generation, \
              task_ids_json,state,armed_at,wake_at,internal_prompt_id,internal_prompt_marker,version,created_at,updated_at) \
             VALUES ('{continuation_id}',1,'session-1',{generation},1,'[\"task-1\"]','{state}', \
                     '2026-07-19','2026-07-19','{internal_prompt_id}','{internal_prompt_marker}',0, \
                     '2026-07-19','2026-07-19')"
        ))
    }

    #[tokio::test]
    async fn migration_creates_continuation_table_and_active_indexes() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        Migrator::up(&db, None).await.unwrap();
        seed_parent_conversation(&db).await;

        db.execute(continuation_insert(
            "continuation-1",
            1,
            "waiting",
            "prompt-1",
            "marker-1",
        ))
        .await
        .unwrap();
        assert!(db
            .execute(continuation_insert(
                "continuation-2",
                2,
                "arming",
                "prompt-2",
                "marker-2"
            ))
            .await
            .is_err());

        db.execute(sql(
            "UPDATE delegation_continuations SET state = 'completed' WHERE continuation_id = 'continuation-1'",
        ))
        .await
        .unwrap();
        db.execute(continuation_insert(
            "continuation-2",
            2,
            "arming",
            "prompt-2",
            "marker-2",
        ))
        .await
        .unwrap();

        assert!(db
            .execute(continuation_insert(
                "continuation-3",
                2,
                "completed",
                "prompt-3",
                "marker-3"
            ))
            .await
            .is_err());
        assert!(db
            .execute(continuation_insert(
                "continuation-3",
                3,
                "completed",
                "prompt-2",
                "marker-3"
            ))
            .await
            .is_err());
        assert!(db
            .execute(continuation_insert(
                "continuation-3",
                3,
                "completed",
                "prompt-3",
                "marker-2"
            ))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn continuation_migration_down_removes_table() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        Migrator::up(&db, None).await.unwrap();

        Migration.down(&SchemaManager::new(&db)).await.unwrap();

        assert!(db
            .query_one(sql("SELECT 1 FROM delegation_continuations"))
            .await
            .is_err());
    }
}
