use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // SQLite ALTER TABLE only accepts one column per statement, so we
        // run two separate alter_table calls instead of chaining add_column.
        manager
            .alter_table(
                Table::alter()
                    .table(Folder::Table)
                    .add_column(ColumnDef::new(Folder::ConnectionId).string().null())
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Folder::Table)
                    .add_column(ColumnDef::new(Folder::RemotePath).string().null())
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Folder::Table)
                    .drop_column(Folder::RemotePath)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(Folder::Table)
                    .drop_column(Folder::ConnectionId)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Folder {
    Table,
    ConnectionId,
    RemotePath,
}
