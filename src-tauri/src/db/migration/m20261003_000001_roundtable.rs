//! Roundtable logical model. Ordinary session tables are not modified.
//!
//! SQLite migrations are not wrapped by sea-orm, so this migration commits the
//! schema in its own transaction. A failed statement rolls that transaction
//! back and leaves the migration unrecorded. The product gate stays off.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let txn = conn.begin().await?;
        if let Err(err) = crate::roundtable::apply_roundtable_schema(&txn).await {
            let _ = txn.rollback().await;
            return Err(err);
        }
        txn.commit().await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let txn = conn.begin().await?;
        if let Err(err) = crate::roundtable::drop_roundtable_schema(&txn).await {
            let _ = txn.rollback().await;
            return Err(err);
        }
        txn.commit().await
    }
}
