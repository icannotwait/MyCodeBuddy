use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Conversation::Table)
                    .add_column(ColumnDef::new(Conversation::AwaitingReplyToken).text())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Conversation::Table)
                    .drop_column(Conversation::AwaitingReplyToken)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Conversation {
    Table,
    AwaitingReplyToken,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

    #[tokio::test]
    async fn up_adds_nullable_awaiting_reply_token_defaulting_null() {
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("open in-memory sqlite");
        conn.execute_unprepared(
            "CREATE TABLE conversation (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT)",
        )
        .await
        .expect("create stub table");
        conn.execute_unprepared("INSERT INTO conversation (title) VALUES ('legacy')")
            .await
            .expect("insert legacy row");

        Migration
            .up(&SchemaManager::new(&conn))
            .await
            .expect("run migration");

        let row = conn
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT awaiting_reply_token FROM conversation".to_owned(),
            ))
            .await
            .expect("query token")
            .expect("legacy row");
        let token: Option<String> = row
            .try_get("", "awaiting_reply_token")
            .expect("token column");
        assert!(token.is_none());
    }
}
