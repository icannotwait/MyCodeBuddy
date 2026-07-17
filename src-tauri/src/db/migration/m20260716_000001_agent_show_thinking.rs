use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(AgentSetting::Table)
                    .add_column(
                        ColumnDef::new(AgentSetting::ShowThinking)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(AgentSetting::Table)
                    .drop_column(AgentSetting::ShowThinking)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum AgentSetting {
    Table,
    ShowThinking,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

    #[tokio::test]
    async fn existing_agent_rows_migrate_with_thinking_hidden() {
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("open sqlite");
        conn.execute(Statement::from_string(
            DbBackend::Sqlite,
            "CREATE TABLE agent_setting (id INTEGER PRIMARY KEY, agent_type TEXT NOT NULL)"
                .to_string(),
        ))
        .await
        .expect("create old schema");
        conn.execute(Statement::from_string(
            DbBackend::Sqlite,
            "INSERT INTO agent_setting (id, agent_type) VALUES (1, 'codex')".to_string(),
        ))
        .await
        .expect("seed old row");

        Migration
            .up(&SchemaManager::new(&conn))
            .await
            .expect("run migration");

        let row = conn
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT show_thinking FROM agent_setting WHERE id = 1".to_string(),
            ))
            .await
            .expect("query migrated row")
            .expect("migrated row");
        let show_thinking: bool = row
            .try_get("", "show_thinking")
            .expect("read show_thinking");
        assert!(!show_thinking);
    }
}
