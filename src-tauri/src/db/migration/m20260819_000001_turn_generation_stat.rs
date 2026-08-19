//! Per-user-turn generation overlay: billed output tokens and generation time
//! measured from live request-usage samples. Parsers do not record this; the
//! live gauge persists it so a reopen can show tok/s and generation share.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(TurnGenerationStat::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(TurnGenerationStat::ConversationId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TurnGenerationStat::UserOrdinal)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TurnGenerationStat::GenerationMs)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TurnGenerationStat::GenerationTokens)
                            .big_integer()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(TurnGenerationStat::ConversationId)
                            .col(TurnGenerationStat::UserOrdinal),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(TurnGenerationStat::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum TurnGenerationStat {
    Table,
    ConversationId,
    UserOrdinal,
    GenerationMs,
    GenerationTokens,
}
