use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // SQLite allows one ADD COLUMN per ALTER TABLE, so three statements.
        manager
            .alter_table(
                Table::alter()
                    .table(WorkTask::Table)
                    // JSON `WorkTaskPendingMerge`: the auto-remerge intent kept
                    // across a conflict-repair cycle (merging → queued →
                    // running → review), consumed when the repaired run
                    // settles. Distinct from `merge_state`, which stays the
                    // crash-recovery anchor of an in-flight merge only.
                    .add_column(ColumnDef::new(WorkTask::PendingMerge).text().null())
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(WorkTask::Table)
                    // JSON `WorkTaskPreflight`: outcome of the folder's
                    // configured preflight command run against the worktree
                    // when the task settles into review (the acceptance
                    // red/green light).
                    .add_column(ColumnDef::new(WorkTask::Preflight).text().null())
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(WorkTask::Table)
                    // Archive timestamp for terminal tasks; NULL = on the
                    // board. Any resurrection (retry / requeue / return)
                    // clears it.
                    .add_column(
                        ColumnDef::new(WorkTask::ArchivedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(WorkTask::Table)
                    .drop_column(WorkTask::ArchivedAt)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(WorkTask::Table)
                    .drop_column(WorkTask::Preflight)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(WorkTask::Table)
                    .drop_column(WorkTask::PendingMerge)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum WorkTask {
    Table,
    PendingMerge,
    Preflight,
    ArchivedAt,
}
