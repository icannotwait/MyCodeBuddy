use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // `title_locked` marks a conversation whose title the user set by hand
        // (rename). Once true, the per-turn auto-title backfill in
        // `get_folder_conversation` must never overwrite it. New rows default to
        // false (eligible for auto-derivation).
        manager
            .alter_table(
                Table::alter()
                    .table(Conversation::Table)
                    .add_column(
                        ColumnDef::new(Conversation::TitleLocked)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await?;

        // Legacy rows predate the flag, and `update_title` (manual rename)
        // already existed, so some of these titles were hand-picked. We cannot
        // tell a hand-picked title from an auto-derived one after the fact, so
        // conservatively LOCK every pre-existing row that already has a title —
        // the auto-backfill must never silently replace a title a user may have
        // chosen before this column existed. Rows with no title stay unlocked so
        // the backfill can fill them in.
        manager
            .exec_stmt(
                Query::update()
                    .table(Conversation::Table)
                    .value(Conversation::TitleLocked, true)
                    .and_where(Expr::col(Conversation::Title).is_not_null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Conversation::Table)
                    .drop_column(Conversation::TitleLocked)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Conversation {
    Table,
    Title,
    TitleLocked,
}

#[cfg(any(not(codeg_test_shard), codeg_test_shard = "1"))]
#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

    /// Drive this migration's `up` against a minimal stand-in of the
    /// `conversation` table and assert the legacy backfill locks exactly the
    /// rows that already have a title.
    #[tokio::test]
    async fn up_locks_preexisting_titled_rows_only() {
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("open in-memory sqlite");
        conn.execute_unprepared(
            "CREATE TABLE conversation (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT)",
        )
        .await
        .expect("create stub table");
        conn.execute_unprepared("INSERT INTO conversation (title) VALUES ('hand named')")
            .await
            .expect("insert titled row");
        conn.execute_unprepared("INSERT INTO conversation (title) VALUES (NULL)")
            .await
            .expect("insert untitled row");

        Migration
            .up(&SchemaManager::new(&conn))
            .await
            .expect("run migration up");

        let rows = conn
            .query_all(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT id, title_locked FROM conversation ORDER BY id".to_owned(),
            ))
            .await
            .expect("query rows");
        assert_eq!(rows.len(), 2);
        let titled: i32 = rows[0].try_get("", "title_locked").expect("titled flag");
        let untitled: i32 = rows[1].try_get("", "title_locked").expect("untitled flag");
        assert_eq!(titled, 1, "a pre-existing titled row must be locked");
        assert_eq!(untitled, 0, "a titleless row must stay unlocked");
    }
}
