use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(CustomAgent::Table)
                    // Whether the agent accepts MCP servers over the ACP wire
                    // (`session/new`'s `mcpServers`) — for a custom agent that
                    // means the built-in codeg-mcp companion. Some ACP agents
                    // fail session creation on any entry there, so the user can
                    // turn it off. Defaults on: that is what every row stored
                    // before this column did, and what almost every agent
                    // accepts.
                    .add_column(
                        ColumnDef::new(CustomAgent::SupportsMcp)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(CustomAgent::Table)
                    .drop_column(CustomAgent::SupportsMcp)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum CustomAgent {
    Table,
    SupportsMcp,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

    /// Agents registered before this column existed were all forwarded MCP, so
    /// the migration must leave them that way — a default of `false` would
    /// silently drop the codeg-mcp companion (delegation, live feedback, task
    /// tools) out of every existing custom agent's session.
    #[tokio::test]
    async fn up_defaults_existing_rows_to_forwarding_mcp() {
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("open in-memory sqlite");
        conn.execute_unprepared(
            "CREATE TABLE custom_agent (id INTEGER PRIMARY KEY AUTOINCREMENT, registry_id TEXT)",
        )
        .await
        .expect("create stub table");
        conn.execute_unprepared("INSERT INTO custom_agent (registry_id) VALUES ('qwen-code')")
            .await
            .expect("insert row");

        Migration
            .up(&SchemaManager::new(&conn))
            .await
            .expect("run migration up");

        let rows = conn
            .query_all(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT supports_mcp FROM custom_agent".to_owned(),
            ))
            .await
            .expect("query rows");
        assert_eq!(rows.len(), 1);
        let supports_mcp: bool = rows[0]
            .try_get("", "supports_mcp")
            .expect("supports_mcp col");
        assert!(supports_mcp, "pre-existing rows must keep forwarding MCP");
    }
}
