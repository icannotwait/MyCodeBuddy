//! Rebuild child-question attention as typed durable attention and create the
//! workflow event outbox atomically.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{DbBackend, TransactionTrait};

use super::completion_rebuild::{self, RebuildFailpoint};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        if conn.get_database_backend() != DbBackend::Sqlite {
            return Err(DbErr::Custom(
                "typed completion attention migration requires SQLite".to_owned(),
            ));
        }

        #[cfg(any(test, feature = "test-utils"))]
        let failpoint = completion_rebuild::configured_failpoint(conn).await?;
        #[cfg(not(any(test, feature = "test-utils")))]
        let failpoint = RebuildFailpoint::None;

        let transaction = conn.begin().await?;
        let result = migrate_in_transaction(&transaction, failpoint).await;
        match result {
            Ok(()) => transaction.commit().await,
            Err(error) => match transaction.rollback().await {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(DbErr::Custom(format!(
                    "{error}; transaction rollback also failed: {rollback_error}"
                ))),
            },
        }
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Typed attention and queued workflow events are durable audit state.
        let _ = manager;
        Ok(())
    }
}

async fn migrate_in_transaction<C: ConnectionTrait>(
    conn: &C,
    failpoint: RebuildFailpoint,
) -> Result<(), DbErr> {
    completion_rebuild::rebuild_attention_requests_and_outbox(conn, failpoint).await
}
