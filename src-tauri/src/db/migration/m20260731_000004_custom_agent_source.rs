use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // SQLite allows one ADD COLUMN per ALTER TABLE, so two statements.
        manager
            .alter_table(
                Table::alter()
                    .table(CustomAgent::Table)
                    // Where the definition came from: `registry` (added from
                    // the public ACP registry) or `manual` (hand-written in
                    // the manual form). Pre-existing rows cannot be told
                    // apart retroactively; `registry` keeps their current
                    // version-status display unchanged.
                    .add_column(
                        ColumnDef::new(CustomAgent::Source)
                            .text()
                            .not_null()
                            .default("registry"),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(CustomAgent::Table)
                    // Optional user-supplied command that prints the locally
                    // installed version (e.g. `qwen --version`). NULL means
                    // "probe with the agent command plus --version".
                    .add_column(ColumnDef::new(CustomAgent::VersionProbe).text().null())
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(CustomAgent::Table)
                    .drop_column(CustomAgent::VersionProbe)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(CustomAgent::Table)
                    .drop_column(CustomAgent::Source)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum CustomAgent {
    Table,
    Source,
    VersionProbe,
}
