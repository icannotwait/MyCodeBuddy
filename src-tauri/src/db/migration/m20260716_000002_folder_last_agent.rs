use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Folder::Table)
                    .add_column(ColumnDef::new(Folder::LastAgentType).text().null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Folder::Table)
                    .drop_column(Folder::LastAgentType)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Folder {
    Table,
    LastAgentType,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

    #[tokio::test]
    async fn existing_folders_migrate_without_implicit_recency() {
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("open sqlite");
        conn.execute(Statement::from_string(
            DbBackend::Sqlite,
            "CREATE TABLE folder (id INTEGER PRIMARY KEY, name TEXT NOT NULL)".to_string(),
        ))
        .await
        .expect("create old schema");
        conn.execute(Statement::from_string(
            DbBackend::Sqlite,
            "INSERT INTO folder (id, name) VALUES (1, 'repo')".to_string(),
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
                "SELECT last_agent_type FROM folder WHERE id = 1".to_string(),
            ))
            .await
            .expect("query migrated row")
            .expect("migrated row");
        let last_agent_type: Option<String> = row
            .try_get("", "last_agent_type")
            .expect("read last_agent_type");
        assert_eq!(last_agent_type, None);
    }
}
